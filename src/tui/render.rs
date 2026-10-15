use std::io::Write;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[derive(Clone, Copy, PartialEq, Default, Debug)]
pub enum Color {
    #[default]
    Default,
    Ansi(u8),
    Rgb(u32),
}

#[derive(Clone, Copy, PartialEq, Default, Debug)]
pub struct Style {
    pub fg: Color,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub selected: bool,
}

impl Style {
    pub const fn fg(n: u8) -> Style {
        Style { fg: Color::Ansi(n), ..PLAIN }
    }
    pub const fn rgb(rgb: u32) -> Style {
        Style { fg: Color::Rgb(rgb), ..PLAIN }
    }
    pub const fn bold(self) -> Style {
        Style { bold: true, ..self }
    }
    pub const fn italic(self) -> Style {
        Style { italic: true, ..self }
    }
    pub const fn underline(self) -> Style {
        Style { underline: true, ..self }
    }

    fn sgr(&self) -> String {
        let mut codes = vec!["0".to_string()];
        match self.fg {
            _ if self.selected => codes.push("38;2;0;0;0;48;2;255;255;255".into()),
            Color::Default => {}
            Color::Ansi(n) => codes.push(format!("38;5;{n}")),
            Color::Rgb(rgb) => codes.push(format!("38;2;{};{};{}", rgb >> 16, (rgb >> 8) & 0xff, rgb & 0xff)),
        }
        if self.bold {
            codes.push("1".into());
        }
        if self.italic {
            codes.push("3".into());
        }
        if self.underline {
            codes.push("4".into());
        }
        format!("\x1b[{}m", codes.join(";"))
    }
}

pub const PLAIN: Style = Style { fg: Color::Default, bold: false, italic: false, underline: false, selected: false };

#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    pub text: String,
    pub style: Style,
    /// The whole URL this text is part of, set by `wrap` so a URL split across rows opens in full.
    pub link: Option<String>,
}

pub type Line = Vec<Span>;

pub fn span(text: impl Into<String>, style: Style) -> Span {
    Span { text: text.into(), style, link: None }
}

pub fn width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

pub fn line_width(line: &Line) -> usize {
    line.iter().map(|s| width(&s.text)).sum()
}

/// Marks where the terminal cursor sits in a rendered line. Zero-width (deprecated LANGUAGE TAG), so wrapping and truncation ignore it.
pub const CURSOR: char = '\u{E0001}';

/// Removes the first `CURSOR` marker from `lines` and returns its (column, row).
pub fn take_cursor(lines: &mut [Line]) -> Option<(usize, usize)> {
    for (row, line) in lines.iter_mut().enumerate() {
        let mut column = 0;
        for span in line.iter_mut() {
            if let Some(at) = span.text.find(CURSOR) {
                span.text.remove(at);
                return Some((column + width(&span.text[..at]), row));
            }
            column += width(&span.text);
        }
    }
    None
}

/// Word-wraps styled spans to `max` columns. Each output line starts with `indent`; long words break mid-word.
pub fn wrap(spans: &[Span], max: usize, indent: &Line) -> Vec<Line> {
    let room = max.saturating_sub(line_width(indent)).max(1);
    let mut lines: Vec<Line> = Vec::new();
    let mut current: Line = Vec::new();
    let mut used = 0;
    for piece in spans.iter().flat_map(split_words).flat_map(mark_links) {
        if piece.text == "\n" {
            lines.push(std::mem::take(&mut current));
            used = 0;
            continue;
        }
        let piece_width = width(&piece.text);
        let is_space = piece.text.trim().is_empty();
        if used + piece_width > room && used > 0 {
            lines.push(std::mem::take(&mut current));
            used = 0;
            if is_space {
                continue;
            }
        }
        if piece_width <= room {
            used += piece_width;
            push_span(&mut current, piece);
            continue;
        }
        for ch in piece.text.chars() {
            let w = ch.width().unwrap_or(0);
            if used + w > room {
                lines.push(std::mem::take(&mut current));
                used = 0;
            }
            used += w;
            push_span(&mut current, Span { text: ch.to_string(), ..piece.clone() });
        }
    }
    if !current.is_empty() || lines.is_empty() {
        lines.push(current);
    }
    lines.into_iter().map(|line| [indent.clone(), line].concat()).collect()
}

fn split_words(s: &Span) -> Vec<Span> {
    let mut out = Vec::new();
    let mut word = String::new();
    for ch in s.text.chars() {
        if ch == ' ' || ch == '\n' {
            if !word.is_empty() {
                out.push(span(std::mem::take(&mut word), s.style));
            }
            out.push(span(ch.to_string(), s.style));
        } else {
            word.push(ch);
        }
    }
    if !word.is_empty() {
        out.push(span(word, s.style));
    }
    out
}

/// Splits a word so each http(s) URL in it is its own underlined span carrying the URL.
fn mark_links(word: Span) -> Vec<Span> {
    let mut out = Vec::new();
    let mut rest = word.text.as_str();
    while let Some((before, url, after)) = split_link(rest) {
        if !before.is_empty() {
            out.push(span(before, word.style));
        }
        out.push(Span { text: url.to_string(), style: word.style.underline(), link: Some(url.to_string()) });
        rest = after;
    }
    if !rest.is_empty() || out.is_empty() {
        out.push(span(rest, word.style));
    }
    out
}

fn push_span(line: &mut Line, piece: Span) {
    match line.last_mut() {
        Some(last) if last.style == piece.style && last.link == piece.link => last.text.push_str(&piece.text),
        _ => line.push(piece),
    }
}

/// Cuts a line to `max` columns, ending with "…" when it had to cut.
pub fn truncate(line: &Line, max: usize) -> Line {
    if line_width(line) <= max {
        return line.clone();
    }
    let mut out = Vec::new();
    let mut used = 0;
    for s in line {
        let mut text = String::new();
        for ch in s.text.chars() {
            let w = ch.width().unwrap_or(0);
            if used + w + 1 > max {
                out.push(span(text, s.style));
                out.push(span("…", s.style));
                return out;
            }
            used += w;
            text.push(ch);
        }
        out.push(span(text, s.style));
    }
    out
}

/// Splits `line` into the parts before, inside and after display columns `from..to`.
fn split_columns(line: &Line, from: usize, to: usize) -> [Line; 3] {
    let mut parts: [Line; 3] = Default::default();
    let mut column = 0;
    for s in line {
        for ch in s.text.chars() {
            let part = if column < from { 0 } else if column < to { 1 } else { 2 };
            match parts[part].last_mut() {
                Some(last) if last.style == s.style => last.text.push(ch),
                _ => parts[part].push(span(ch.to_string(), s.style)),
            }
            column += ch.width().unwrap_or(0);
        }
    }
    parts
}

/// `line` with display columns `from..to` drawn as selected (black on white).
pub fn select(line: &Line, from: usize, to: usize) -> Line {
    let [mut out, mut inside, after] = split_columns(line, from, to);
    for s in &mut inside {
        s.style.selected = true;
    }
    out.extend(inside);
    out.extend(after);
    out
}

/// The plain text in display columns `from..to` of `line`.
pub fn text_in(line: &Line, from: usize, to: usize) -> String {
    let [_, inside, _] = split_columns(line, from, to);
    inside.iter().map(|s| s.text.as_str()).collect()
}

/// ANSI for `line`; http(s) URLs are underlined wherever they appear.
fn to_ansi(line: &Line) -> String {
    let mut out = String::new();
    for s in line {
        let text = s.text.replace('\t', "    ");
        let mut rest = text.as_str();
        while let Some((before, url, after)) = split_link(rest) {
            push_styled(&mut out, s.style, before);
            push_styled(&mut out, s.style.underline(), url);
            rest = after;
        }
        push_styled(&mut out, s.style, rest);
    }
    out.push_str("\x1b[0m");
    out
}

fn push_styled(out: &mut String, style: Style, text: &str) {
    if !text.is_empty() {
        out.push_str(&style.sgr());
        out.push_str(text);
    }
}

/// `text` split around its first http(s) URL, without trailing punctuation or an unmatched `)`.
fn split_link(text: &str) -> Option<(&str, &str, &str)> {
    let start = [text.find("https://"), text.find("http://")].into_iter().flatten().min()?;
    let tail = &text[start..];
    let mut url = &tail[..tail.find(|c: char| c.is_whitespace() || "<>\"'`".contains(c)).unwrap_or(tail.len())];
    while let Some(last) = url.chars().last() {
        let unmatched_paren = last == ')' && url.matches(')').count() > url.matches('(').count();
        if !".,;:!?".contains(last) && !unmatched_paren {
            break;
        }
        url = &url[..url.len() - 1];
    }
    Some((&text[..start], url, &text[start + url.len()..]))
}

/// The http(s) URL covering display column `column` of `line`, whole even when `wrap` split it across rows.
pub fn link_at(line: &Line, column: usize) -> Option<String> {
    let mut col = 0;
    for s in line {
        let start = col;
        col += s.text.width();
        if !(start..col).contains(&column) {
            continue;
        }
        if let Some(link) = &s.link {
            return Some(link.clone());
        }
        return url_at(&s.text, column - start).map(str::to_string);
    }
    None
}

fn url_at(text: &str, column: usize) -> Option<&str> {
    let mut rest = text;
    let mut col = 0;
    while let Some((before, url, after)) = split_link(rest) {
        let start = col + before.width();
        col = start + url.width();
        if (start..col).contains(&column) {
            return Some(url);
        }
        rest = after;
    }
    None
}

/// Keeps the last frame and rewrites only the rows that changed.
pub struct Screen {
    previous: Vec<String>,
}

impl Screen {
    pub fn new() -> Screen {
        Screen { previous: Vec::new() }
    }

    pub fn invalidate(&mut self) {
        self.previous.clear();
    }

    pub fn draw(&mut self, out: &mut impl Write, rows: Vec<Line>, cursor: Option<(u16, u16)>) -> std::io::Result<()> {
        let rows: Vec<String> = rows.iter().map(to_ansi).collect();
        let mut buffer = String::from("\x1b[?2026h\x1b[?25l");
        for (y, row) in rows.iter().enumerate() {
            if self.previous.get(y) == Some(row) {
                continue;
            }
            // Erase before writing: EL after a full-width row hits the pending-wrap cell and erases the last glyph.
            buffer.push_str(&format!("\x1b[{};1H\x1b[2K{row}", y + 1));
        }
        if rows.len() < self.previous.len() {
            buffer.push_str(&format!("\x1b[{};1H\x1b[J", rows.len() + 1));
        }
        if let Some((x, y)) = cursor {
            buffer.push_str(&format!("\x1b[{};{}H\x1b[?25h", y + 1, x + 1));
        }
        buffer.push_str("\x1b[?2026l");
        out.write_all(buffer.as_bytes())?;
        out.flush()?;
        self.previous = rows;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_marker_becomes_a_column_and_is_removed() {
        let mut lines = vec![vec![span("title", PLAIN)], vec![span("> ", PLAIN), span(format!("日本{CURSOR}x"), PLAIN)]];
        assert_eq!(take_cursor(&mut lines), Some((6, 1)));
        assert_eq!(lines[1][1].text, "日本x");
    }

    #[test]
    fn urls_are_underlined_without_trailing_punctuation() {
        assert_eq!(split_link("see https://x.dev/a_(b)). ok"), Some(("see ", "https://x.dev/a_(b)", "). ok")));
        assert_eq!(split_link("plain text"), None);
        let ansi = to_ansi(&vec![span("go http://a.b, now", PLAIN)]);
        assert_eq!(ansi, "\x1b[0mgo \x1b[0;4mhttp://a.b\x1b[0m, now\x1b[0m");
    }

    #[test]
    fn link_at_finds_the_url_under_a_column() {
        let line = vec![span("世界 https://a.dev, http://b.dev", PLAIN)];
        assert_eq!(link_at(&line, 5).as_deref(), Some("https://a.dev"));
        assert_eq!(link_at(&line, 18), None);
        assert_eq!(link_at(&line, 20).as_deref(), Some("http://b.dev"));
        assert_eq!(link_at(&line, 2), None);
    }

    #[test]
    fn a_wrapped_url_is_underlined_and_opens_whole_on_every_row() {
        let rows = wrap(&[span("go https://a.dev/long", PLAIN)], 10, &vec![]);
        assert_eq!(text(&rows), ["go ", "https://a.", "dev/long"]);
        assert!(to_ansi(&rows[2]).starts_with("\x1b[0;4mdev/long"));
        assert_eq!(link_at(&rows[2], 0).as_deref(), Some("https://a.dev/long"));
    }

    fn text(lines: &[Line]) -> Vec<String> {
        lines.iter().map(|l| l.iter().map(|s| s.text.as_str()).collect()).collect()
    }

    #[test]
    fn wrap_breaks_on_words_and_drops_the_break_space() {
        let lines = wrap(&[span("aaa bbb ccc", PLAIN)], 7, &vec![]);
        assert_eq!(text(&lines), ["aaa bbb", "ccc"]);
    }

    #[test]
    fn wrap_keeps_indent_and_splits_long_words() {
        let lines = wrap(&[span("abcdefgh", PLAIN)], 5, &vec![span(" ", PLAIN)]);
        assert_eq!(text(&lines), [" abcd", " efgh"]);
    }

    #[test]
    fn truncate_counts_wide_chars() {
        let line = vec![span("ąę中文abc", PLAIN)];
        assert_eq!(text(&[truncate(&line, 6)]), ["ąę中…"]);
    }
}
