use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::tools::{MAX_BYTES, MAX_LINES, format_size};

// Two-letter pieces (sorted), then a row-major bitset of valid piece pairs, packed from pi's anchor table.
const TABLE: &[u8] = include_bytes!("anchors.bin");
const PIECES: usize = 1360;
const ROW_BYTES: usize = PIECES / 8;
const ANCHOR_COUNT: u32 = 1_353_139;
const PROBE_STRIDE: u32 = 836_286;

/// Session-wide anchor registry: every served line owns a unique 4-letter anchor.
pub struct Anchors {
    row_starts: Vec<u32>,
    probe: u32,
    owner: HashMap<String, PathBuf>,
    minted: HashSet<String>,
    files: HashMap<PathBuf, Snapshot>,
    /// One level per file, like pi: the bytes before and after the last anchored edit.
    undo: HashMap<PathBuf, (Vec<u8>, Vec<u8>)>,
}

#[derive(Default)]
struct Snapshot {
    lines: Vec<String>,
    anchors: Vec<String>,
}

struct Change {
    start: usize,
    removed: Vec<(String, String)>,
    added: usize,
}

struct Text {
    lines: Vec<String>,
    bom: bool,
    crlf: bool,
    final_newline: bool,
}

impl Anchors {
    pub fn new(seed: &str) -> Anchors {
        let mut row_starts = Vec::with_capacity(PIECES);
        let mut total = 0;
        for row in 0..PIECES {
            row_starts.push(total);
            total += row_bits(row).iter().map(|b| b.count_ones()).sum::<u32>();
        }
        assert_eq!(total, ANCHOR_COUNT);
        let mut probe: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in seed.bytes() {
            probe = (probe ^ byte as u64).wrapping_mul(0x100_0000_01b3);
        }
        Anchors {
            row_starts,
            probe: (probe % ANCHOR_COUNT as u64) as u32,
            owner: HashMap::new(),
            minted: HashSet::new(),
            files: HashMap::new(),
            undo: HashMap::new(),
        }
    }

    fn anchor_at(&self, index: u32) -> String {
        let row = self.row_starts.partition_point(|&start| start <= index) - 1;
        let mut rank = index - self.row_starts[row];
        let bits = row_bits(row);
        for col in 0..PIECES {
            if bits[col / 8] >> (col % 8) & 1 == 0 {
                continue;
            }
            if rank == 0 {
                return format!("{}{}", piece(row), piece(col));
            }
            rank -= 1;
        }
        unreachable!("anchor index {index} is past its row")
    }

    fn mint(&mut self, path: &Path) -> String {
        for _ in 0..ANCHOR_COUNT {
            self.probe = (self.probe + PROBE_STRIDE) % ANCHOR_COUNT;
            let anchor = self.anchor_at(self.probe);
            if self.minted.insert(anchor.clone()) {
                self.owner.insert(anchor.clone(), path.to_path_buf());
                return anchor;
            }
        }
        panic!("all {ANCHOR_COUNT} anchors are used in this session")
    }

    /// Records `lines` as the content of `path`. Lines in the unchanged prefix and suffix keep their anchors.
    // shortcut: prefix/suffix alignment re-mints anchors between two separate external edits, use a line diff if that hurts.
    fn sync(&mut self, path: &Path, lines: Vec<String>) -> Change {
        let old = self.files.remove(path).unwrap_or_default();
        let prefix = old.lines.iter().zip(&lines).take_while(|(a, b)| a == b).count();
        let max_suffix = old.lines.len().min(lines.len()) - prefix;
        let suffix = old.lines.iter().rev().zip(lines.iter().rev()).take(max_suffix).take_while(|(a, b)| a == b).count();
        let old_end = old.lines.len() - suffix;
        let mut removed = Vec::new();
        for i in prefix..old_end {
            self.owner.remove(&old.anchors[i]);
            removed.push((old.anchors[i].clone(), old.lines[i].clone()));
        }
        let mut anchors = old.anchors[..prefix].to_vec();
        let added = lines.len() - suffix - prefix;
        for _ in 0..added {
            anchors.push(self.mint(path));
        }
        anchors.extend_from_slice(&old.anchors[old_end..]);
        self.files.insert(path.to_path_buf(), Snapshot { lines, anchors });
        Change { start: prefix, removed, added }
    }

    pub fn read(&mut self, cwd: &Path, input: &Value) -> Result<String, String> {
        let path = existing_file(cwd, str_arg(input, &["path"])?)?;
        let text = Text::load(&path)?;
        let empty = text.is_empty();
        self.sync(&path, text.lines);
        let snapshot = &self.files[&path];
        if empty {
            return Ok(format!("{}│\n[File is empty. Use replace to insert content.]", snapshot.anchors[0]));
        }
        let total = snapshot.lines.len();
        let start = match &input["offset"] {
            Value::Null => 0,
            Value::Number(n) => n.as_u64().filter(|&n| n >= 1).ok_or("[E_BAD_SHAPE] Read request field \"offset\" must be a positive integer.")? as usize - 1,
            Value::String(s) if s.parse::<usize>().is_ok_and(|n| n >= 1) => s.parse::<usize>().unwrap() - 1,
            Value::String(anchor) => self.index_of(&path, anchor)? + 1,
            _ => return Err("[E_BAD_SHAPE] offset must be a line number or an anchor.".into()),
        };
        let limit = match &input["limit"] {
            Value::Null => None,
            v => Some(v.as_u64().filter(|&n| n >= 1).ok_or("[E_BAD_SHAPE] Read request field \"limit\" must be a positive integer.")? as usize),
        };
        if start >= total {
            return Ok(format!("Offset {} is beyond end of file ({total} lines total). Use offset=1 to read from the start, or offset={total} to read the last line.", start + 1));
        }
        Ok(self.rows(&path, start, limit))
    }

    /// Serves `path` from `start`, cut at `limit`, MAX_LINES or MAX_BYTES, with a hint to continue.
    fn rows(&self, path: &Path, start: usize, limit: Option<usize>) -> String {
        let snapshot = &self.files[path];
        let total = snapshot.lines.len();
        let end = limit.map_or(total, |limit| (start + limit).min(total));
        let mut out = String::new();
        let mut shown = start;
        let mut byte_limited = false;
        while shown < end {
            let row = row_text(&snapshot.anchors[shown], &snapshot.lines[shown], shown + 1, path);
            if shown - start == MAX_LINES || out.len() + row.len() + 1 > MAX_BYTES {
                byte_limited = shown - start < MAX_LINES;
                break;
            }
            if shown > start {
                out.push('\n');
            }
            out.push_str(&row);
            shown += 1;
        }
        if shown < total {
            let size = if byte_limited { format!(" ({} limit)", format_size(MAX_BYTES)) } else { String::new() };
            let last = &snapshot.anchors[shown - 1];
            out.push_str(&format!("\n\n[Showing lines {}-{shown} of {total}{size}. Use offset={last} to continue.]", start + 1));
        }
        out
    }

    pub fn written(&mut self, path: &Path) -> Result<String, String> {
        let text = Text::load(path)?;
        self.sync(path, text.lines);
        Ok(self.rows(path, 0, None))
    }

    pub fn replace(&mut self, input: &Value) -> Result<String, String> {
        let from = str_arg(input, &["remove_from", "replace_from"])?;
        let to = str_arg(input, &["remove_to", "replace_to"])?;
        let text = str_arg(input, &["text"])?;
        let (path, mut file, first, last) = self.resolve_range(from, to)?;
        let at_end = last + 1 == file.lines.len();
        file.lines.splice(first..=last, split_text(text));
        file.final_newline |= at_end && text.ends_with('\n');
        self.commit(&path, file)
    }

    pub fn replace_match(&mut self, input: &Value) -> Result<String, String> {
        let from = str_arg(input, &["replace_from", "remove_from"])?;
        let to = str_arg(input, &["replace_to", "remove_to"])?;
        let old = str_arg(input, &["old_string"])?;
        let new = str_arg(input, &["new_string"])?;
        if old.is_empty() {
            return Err("[E_BAD_SHAPE] old_string must not be empty.".into());
        }
        let (path, mut file, first, last) = self.resolve_range(from, to)?;
        let region = file.lines[first..=last].join("\n");
        if !region.contains(old) {
            return Err(format!("[E_NO_MATCH] old_string was not found in lines {}-{} of {}. Copy the fragment exactly from the served rows.", first + 1, last + 1, path.display()));
        }
        let replaced = region.replace(old, new);
        file.lines.splice(first..=last, replaced.split('\n').map(String::from));
        self.commit(&path, file)
    }

    pub fn insert(&mut self, input: &Value) -> Result<String, String> {
        let anchor = str_arg(input, &["anchor"])?;
        let text = str_arg(input, &["text"])?;
        let after = direction_is_after(input)?;
        let (path, mut file, index, _) = self.resolve_range(anchor, anchor)?;
        let lines = if text.is_empty() { vec![String::new()] } else { split_text(text) };
        let at = if after { index + 1 } else { index };
        let at_end = at == file.lines.len();
        file.lines.splice(at..at, lines);
        file.final_newline |= at_end && text.ends_with('\n');
        self.commit(&path, file)
    }

    /// Duplicates the source range next to `anchor`, in the same or another file; the copies get fresh anchors.
    /// shortcut: copied lines take the destination's line endings, not their own; track per-line endings if mixed files matter.
    pub fn copy(&mut self, input: &Value) -> Result<String, String> {
        let (source, source_file, first, last) = self.resolve_range(str_arg(input, &["source_from"])?, str_arg(input, &["source_to"])?)?;
        let (path, mut file, at) = self.destination(input)?;
        if path == source {
            refuse_inside(at, first, last)?;
        }
        file.lines.splice(at..at, source_file.lines[first..=last].iter().cloned());
        self.commit(&path, file)
    }

    /// Moves the source range next to `anchor`; across files the destination is written first, then the source.
    pub fn move_lines(&mut self, input: &Value) -> Result<String, String> {
        let (source, mut source_file, first, last) = self.resolve_range(str_arg(input, &["source_from"])?, str_arg(input, &["source_to"])?)?;
        let (path, mut file, at) = self.destination(input)?;
        let block: Vec<String> = source_file.lines.drain(first..=last).collect();
        if path == source {
            refuse_inside(at, first, last)?;
            let at = if at > last { at - block.len() } else { at };
            source_file.lines.splice(at..at, block);
            return self.commit(&path, source_file);
        }
        let count = block.len();
        file.lines.splice(at..at, block);
        let destination_diff = self.commit(&path, file)?;
        let source_diff = self.commit(&source, source_file)?;
        Ok(format!(
            "Successfully moved {count} line(s) from {} to {}.\n\n--- {0} ---\n{source_diff}\n\n--- {1} ---\n{destination_diff}",
            source.display(),
            path.display()
        ))
    }

    /// Restores the bytes from before the last anchored edit of `path`, unless the file changed since.
    pub fn undo(&mut self, cwd: &Path, input: &Value) -> Result<String, String> {
        let raw = str_arg(input, &["path"])?;
        let path = resolve(cwd, raw);
        let path = path.canonicalize().unwrap_or(path);
        let Some((before, after)) = self.undo.get(&path).cloned() else {
            return Err(format!("[E_UNDO_NONE] No undo history for {raw}."));
        };
        let current = std::fs::read(&path).ok();
        if current.as_ref().is_some_and(|bytes| *bytes != after) {
            return Err(format!("[E_UNDO_STALE] Cannot undo last change on {raw}: the file was modified after the edit, so nothing was reverted and the file was left untouched. The undo record is kept. Do not edit the file to force the undo. Call read() to inspect the current state."));
        }
        std::fs::write(&path, before).map_err(|e| format!("Cannot write {}: {e}", path.display()))?;
        self.undo.remove(&path);
        let change = self.sync(&path, Text::load(&path)?.lines);
        let mut parts = vec![format!("Undone last change on {raw}.")];
        if current.is_none() {
            parts.push("The file was deleted; restored it from undo history.".into());
        }
        if change.added > 0 || !change.removed.is_empty() {
            parts.push(format!("Removed {} line(s), restored {} line(s).", change.removed.len(), change.added));
        }
        parts.push("Call read for fresh anchors.".into());
        Ok(parts.join("\n"))
    }

    /// The file `anchor` belongs to and the line index a block lands on, from `direction`.
    fn destination(&mut self, input: &Value) -> Result<(PathBuf, Text, usize), String> {
        let anchor = str_arg(input, &["anchor"])?;
        let after = direction_is_after(input)?;
        let (path, file, index, _) = self.resolve_range(anchor, anchor)?;
        Ok((path, file, if after { index + 1 } else { index }))
    }

    /// Finds the file owning both anchors, re-reads it, and returns the line range they address.
    fn resolve_range(&mut self, from: &str, to: &str) -> Result<(PathBuf, Text, usize, usize), String> {
        let path = self.owner_of(from)?;
        if self.owner_of(to)? != path {
            return Err(format!("[E_BAD_SHAPE] \"{from}\" and \"{to}\" belong to different files."));
        }
        let file = Text::load(&path)?;
        self.sync(&path, file.lines.clone());
        let (a, b) = (self.index_of(&path, from)?, self.index_of(&path, to)?);
        Ok((path, file, a.min(b), a.max(b)))
    }

    pub fn owner_of(&self, anchor: &str) -> Result<PathBuf, String> {
        self.owner.get(anchor).cloned().ok_or_else(|| format!("[E_STALE_ANCHOR] \"{anchor}\" is not owned in this session. Call read() on the target file first."))
    }

    fn index_of(&self, path: &Path, anchor: &str) -> Result<usize, String> {
        let stale = || format!("[E_STALE_ANCHOR] \"{anchor}\" is stale in {0}: the file changed since read. Call read() on {0} for fresh anchors.", path.display());
        match self.owner.get(anchor) {
            Some(owner) if owner != path => return Err(format!("[E_STALE_ANCHOR] \"{anchor}\" is owned by {}. Read {0} to use that anchor.", owner.display())),
            None => return Err(stale()),
            Some(_) => {}
        }
        self.files[path].anchors.iter().position(|a| a == anchor).ok_or_else(stale)
    }

    fn commit(&mut self, path: &Path, mut file: Text) -> Result<String, String> {
        if file.lines.is_empty() {
            file.lines.push(String::new());
            file.final_newline = false;
        }
        if file.lines == self.files[path].lines {
            return Ok(format!("No changes made to {}: the edit produced identical content.", path.display()));
        }
        let before = std::fs::read(path).map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
        let after = file.encode();
        std::fs::write(path, &after).map_err(|e| format!("Cannot write {}: {e}", path.display()))?;
        self.undo.insert(path.to_path_buf(), (before, after.into_bytes()));
        let empty = file.is_empty();
        let change = self.sync(path, file.lines);
        let snapshot = &self.files[path];
        if empty {
            return Ok(format!("File is empty. Use replace on {}│ to insert content.", snapshot.anchors[0]));
        }
        let mut out = Vec::new();
        let start = change.start;
        if start >= 2 {
            out.push(" ...".to_string());
        }
        if start >= 1 {
            out.push(format!(" {}│{}", snapshot.anchors[start - 1], snapshot.lines[start - 1]));
        }
        for (anchor, line) in &change.removed {
            out.push(format!("-{anchor}│{line}"));
        }
        for i in start..start + change.added {
            out.push(format!("+{}│{}", snapshot.anchors[i], snapshot.lines[i]));
        }
        let after = start + change.added;
        if after < snapshot.lines.len() {
            out.push(format!(" {}│{}", snapshot.anchors[after], snapshot.lines[after]));
        }
        if after + 1 < snapshot.lines.len() {
            out.push(" ...".to_string());
        }
        Ok(crate::tools::truncate_head(out.join("\n"), "diff"))
    }
}

impl Text {
    fn load(path: &Path) -> Result<Text, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
        if bytes[..bytes.len().min(8192)].contains(&0) {
            return Err(format!("{} is a binary file.", path.display()));
        }
        let raw = String::from_utf8(bytes).map_err(|_| format!("{} is not UTF-8 text.", path.display()))?;
        let bom = raw.starts_with('\u{feff}');
        let body = raw.trim_start_matches('\u{feff}');
        let crlf = body.contains("\r\n");
        let body = if crlf { body.replace("\r\n", "\n") } else { body.to_string() };
        let final_newline = body.ends_with('\n');
        let mut lines: Vec<String> = body.split('\n').map(String::from).collect();
        if final_newline {
            lines.pop();
        }
        Ok(Text { lines, bom, crlf, final_newline })
    }

    fn encode(&self) -> String {
        let newline = if self.crlf { "\r\n" } else { "\n" };
        let mut out = if self.bom { "\u{feff}".to_string() } else { String::new() };
        out.push_str(&self.lines.join(newline));
        if self.final_newline {
            out.push_str(newline);
        }
        out
    }

    fn is_empty(&self) -> bool {
        self.lines.len() == 1 && self.lines[0].is_empty() && !self.final_newline
    }
}

fn row_bits(row: usize) -> &'static [u8] {
    &TABLE[PIECES * 2 + row * ROW_BYTES..][..ROW_BYTES]
}

fn piece(index: usize) -> &'static str {
    std::str::from_utf8(&TABLE[index * 2..index * 2 + 2]).unwrap()
}

fn row_text(anchor: &str, line: &str, number: usize, path: &Path) -> String {
    let row = format!("{anchor}│{line}");
    if row.len() <= MAX_BYTES {
        return row;
    }
    format!("{anchor}│[Line {number} is {}, exceeds {}; content not shown. Use bash: sed -n '{number}p' {} | head -c {MAX_BYTES}]", format_size(row.len()), format_size(MAX_BYTES), path.display())
}

/// Replacement text to lines: one trailing line break ends the last line instead of adding a blank one.
fn split_text(text: &str) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }
    let text = text.strip_suffix('\n').unwrap_or(text);
    text.split('\n').map(|line| line.strip_suffix('\r').unwrap_or(line).to_string()).collect()
}

fn direction_is_after(input: &Value) -> Result<bool, String> {
    match str_arg(input, &["direction"])? {
        "after" => Ok(true),
        "before" => Ok(false),
        other => Err(format!("[E_BAD_SHAPE] direction must be \"after\" or \"before\", got \"{other}\".")),
    }
}

/// A block landing at index `at` of its own file must not split its source lines `first..=last`.
fn refuse_inside(at: usize, first: usize, last: usize) -> Result<(), String> {
    if at > first && at <= last {
        return Err(format!("[E_BAD_SHAPE] \"anchor\" places the block inside the source range (lines {}-{}). Use a line outside the source range.", first + 1, last + 1));
    }
    Ok(())
}

pub fn str_arg<'a>(input: &'a Value, names: &[&str]) -> Result<&'a str, String> {
    names.iter().find_map(|name| input[name].as_str()).ok_or_else(|| format!("[E_BAD_SHAPE] Missing string field \"{}\".", names[0]))
}

pub fn resolve(cwd: &Path, path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(rest),
        None => cwd.join(path),
    }
}

fn existing_file(cwd: &Path, path: &str) -> Result<PathBuf, String> {
    let full = resolve(cwd, path).canonicalize().map_err(|e| format!("Cannot read {path}: {e}"))?;
    if full.is_dir() {
        return Err(format!("{} is a directory. Use fd or bash ls to list it.", full.display()));
    }
    Ok(full)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temp_file(name: &str, content: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("agent-hashline-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("f.txt");
        std::fs::write(&path, content).unwrap();
        path.canonicalize().unwrap()
    }

    fn anchors_of(output: &str) -> Vec<String> {
        output.lines().filter_map(|row| row.split_once('│')).map(|(a, _)| a.trim_start_matches(['+', '-', ' ']).to_string()).collect()
    }

    #[test]
    fn anchor_table_matches_pi() {
        let anchors = Anchors::new("");
        let picks: Vec<String> = [0, 1, 500_000, 836_286, 1_353_138].iter().map(|&i| anchors.anchor_at(i)).collect();
        assert_eq!(picks, ["AAAA", "AAAB", "PuOb", "ekJR", "zzzz"]);
    }

    #[test]
    fn read_serves_unique_anchors_and_pages_by_anchor() {
        let path = temp_file("read", "a\nb\nc\n");
        let mut anchors = Anchors::new("s");
        let first = anchors.read(Path::new("/"), &json!({"path": path, "limit": 2})).unwrap();
        let served = anchors_of(&first);
        assert_eq!(served.len(), 2);
        assert!(first.ends_with(&format!("[Showing lines 1-2 of 3. Use offset={} to continue.]", served[1])));
        let rest = anchors.read(Path::new("/"), &json!({"path": path, "offset": served[1]})).unwrap();
        assert_eq!(rest.split_once('│').unwrap().1, "c");
    }

    #[test]
    fn replace_keeps_untouched_anchors_and_reports_diff() {
        let path = temp_file("replace", "one\ntwo\nthree\nfour\n");
        let mut anchors = Anchors::new("s");
        let a = anchors_of(&anchors.read(Path::new("/"), &json!({"path": path})).unwrap());
        let diff = anchors.replace(&json!({"remove_from": a[1], "remove_to": a[2], "text": "TWO\n"})).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "one\nTWO\nfour\n");
        let lines: Vec<&str> = diff.lines().collect();
        assert_eq!(lines[0], format!(" {}│one", a[0]));
        assert_eq!(lines[1], format!("-{}│two", a[1]));
        assert_eq!(lines[2], format!("-{}│three", a[2]));
        assert!(lines[3].starts_with('+') && lines[3].ends_with("│TWO"));
        assert_eq!(lines[4], format!(" {}│four", a[3]));
        let stale = anchors.replace(&json!({"remove_from": a[1], "remove_to": a[1], "text": "x"})).unwrap_err();
        assert!(stale.starts_with("[E_STALE_ANCHOR]"));
    }

    #[test]
    fn external_change_keeps_unchanged_anchors_and_refuses_stale_ones() {
        let path = temp_file("external", "one\ntwo\nthree\n");
        let mut anchors = Anchors::new("s");
        let a = anchors_of(&anchors.read(Path::new("/"), &json!({"path": path})).unwrap());
        std::fs::write(&path, "one\n2\nthree\n").unwrap();
        assert!(anchors.replace(&json!({"remove_from": a[1], "remove_to": a[1], "text": "x"})).unwrap_err().contains("changed since read"));
        anchors.replace_match(&json!({"replace_from": a[2], "replace_to": a[2], "old_string": "ee", "new_string": "EE"})).unwrap();
        anchors.insert(&json!({"anchor": a[0], "direction": "before", "text": "zero"})).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "zero\none\n2\nthrEE\n");
    }

    #[test]
    fn crlf_bom_and_missing_final_newline_survive_edits() {
        let path = temp_file("crlf", "\u{feff}a\r\nb");
        let mut anchors = Anchors::new("s");
        let a = anchors_of(&anchors.read(Path::new("/"), &json!({"path": path})).unwrap());
        anchors.insert(&json!({"anchor": a[0], "direction": "after", "text": "mid"})).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "\u{feff}a\r\nmid\r\nb");
    }

    #[test]
    fn deleting_every_line_leaves_an_empty_line_anchor() {
        let path = temp_file("empty", "a\nb\n");
        let mut anchors = Anchors::new("s");
        let a = anchors_of(&anchors.read(Path::new("/"), &json!({"path": path})).unwrap());
        let out = anchors.replace(&json!({"remove_from": a[0], "remove_to": a[1], "text": ""})).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "");
        let empty = out.split("replace on ").nth(1).unwrap().split('│').next().unwrap();
        anchors.replace(&json!({"remove_from": empty, "remove_to": empty, "text": "x\n"})).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "x\n");
    }

    #[test]
    fn copy_gives_fresh_anchors_and_move_relocates_within_and_across_files() {
        let path = temp_file("copy", "a\nb\nc\n");
        let other = path.with_file_name("g.txt");
        std::fs::write(&other, "x\n").unwrap();
        let mut anchors = Anchors::new("s");
        let a = anchors_of(&anchors.read(Path::new("/"), &json!({"path": path})).unwrap());
        let diff = anchors.copy(&json!({"source_from": a[0], "source_to": a[1], "anchor": a[2], "direction": "after"})).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a\nb\nc\na\nb\n");
        let copied: Vec<String> = diff.lines().filter(|l| l.starts_with('+')).map(|l| anchors_of(l)[0].clone()).collect();
        assert!(copied.len() == 2 && !copied.iter().any(|anchor| a.contains(anchor)));
        let inside = anchors.move_lines(&json!({"source_from": a[0], "source_to": a[2], "anchor": a[1], "direction": "after"})).unwrap_err();
        assert!(inside.contains("inside the source range"));
        anchors.move_lines(&json!({"source_from": a[0], "source_to": a[0], "anchor": a[2], "direction": "after"})).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "b\nc\na\na\nb\n");
        let a = anchors_of(&anchors.read(Path::new("/"), &json!({"path": path})).unwrap());
        let x = anchors_of(&anchors.read(Path::new("/"), &json!({"path": other})).unwrap());
        anchors.move_lines(&json!({"source_from": a[0], "source_to": a[0], "anchor": x[0], "direction": "before"})).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "c\na\na\nb\n");
        assert_eq!(std::fs::read_to_string(&other).unwrap(), "b\nx\n");
    }

    #[test]
    fn undo_restores_bom_and_crlf_and_a_deleted_file_but_refuses_a_changed_one() {
        let path = temp_file("undo", "\u{feff}a\r\nb\r\n");
        let mut anchors = Anchors::new("s");
        let undo = json!({"path": path});
        let a = anchors_of(&anchors.read(Path::new("/"), &undo).unwrap());
        anchors.replace(&json!({"remove_from": a[0], "remove_to": a[0], "text": "A\n"})).unwrap();
        std::fs::write(&path, "changed").unwrap();
        assert!(anchors.undo(Path::new("/"), &undo).unwrap_err().starts_with("[E_UNDO_STALE]"));
        std::fs::remove_file(&path).unwrap();
        let out = anchors.undo(Path::new("/"), &undo).unwrap();
        assert!(out.contains("The file was deleted; restored it from undo history."));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "\u{feff}a\r\nb\r\n");
        assert!(anchors.undo(Path::new("/"), &undo).unwrap_err().starts_with("[E_UNDO_NONE]"));
    }
}
