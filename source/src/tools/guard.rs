// pi's audit of 53k bash calls: 10k were cat/sed -n/head file reads (a third of the "edit without read" failures),
// and blind `sleep N` burned 41h of wall time.
const MIN_BLOCKED_SLEEP_SECS: u64 = 15;

/// Why bash should not run `command` (a plain file read or a blind sleep), or None when it may run.
pub fn bash(command: &str) -> Option<String> {
    if let Some(path) = file_read(command) {
        return Some(format!("Use read({path}) with offset/limit instead of bash; its anchors are what edits use."));
    }
    if blind_sleep(command) {
        return Some("No blind sleep. Server: curl -sf --retry 30 --retry-all-errors --retry-delay 1 URL. CI: gh run watch --exit-status, gh pr checks --watch. Long job: load_tools terminals.".into());
    }
    None
}

/// The path of a lone `cat FILE`, `head [-N] FILE` or `sed -n 'N,Mp' FILE`, joined onto DIR after `cd DIR &&`.
fn file_read(command: &str) -> Option<String> {
    let mut words: Vec<&str> = command.split_whitespace().collect();
    let mut dir = None;
    if let ["cd", cd_dir, "&&", ..] = words.as_slice() {
        dir = Some(*cd_dir);
        words.drain(..3);
    }
    let file = match words.as_slice() {
        ["cat", file] | ["cat", "-n", file] | ["head", file] => file,
        ["head", count, file] if count.strip_prefix("-n").or(count.strip_prefix('-')).is_some_and(is_number) => file,
        ["head", "-n", count, file] if is_number(count) => file,
        ["sed", "-n", range, file] if is_print_range(range) => file,
        _ => return None,
    };
    let file = file.trim_matches(['\'', '"']);
    let path = dir.map_or(file.to_string(), |dir| std::path::Path::new(dir).join(file).display().to_string());
    (!file.starts_with('-') && !path.contains(['|', '<', '>', ';', '&', '$', '`', '*', '?', '[', '{', '('])).then_some(path)
}

/// `Np` or `N,Mp`, bare or quoted.
fn is_print_range(range: &str) -> bool {
    let Some(lines) = range.trim_matches(['\'', '"']).strip_suffix('p') else { return false };
    match lines.split_once(',') {
        Some((first, last)) => is_number(first) && is_number(last),
        None => is_number(lines),
    }
}

/// A `sleep` of MIN_BLOCKED_SLEEP_SECS or more that starts a command before the first loop body.
fn blind_sleep(command: &str) -> bool {
    let command = command.replace('\n', " ; ").replace('(', "( ");
    let words: Vec<&str> = command.split_whitespace().collect();
    let words = &words[..words.iter().position(|word| *word == "do").unwrap_or(words.len())];
    words.iter().enumerate().any(|(index, word)| {
        let starts_command = index == 0
            || words[index - 1].ends_with([';', '&', '|', '(', '{'])
            || matches!(words[index - 1], "then" | "else");
        *word == "sleep" && starts_command && words.get(index + 1).map_or(0, |arg| sleep_seconds(arg)) >= MIN_BLOCKED_SLEEP_SECS
    })
}

/// Seconds of a `sleep` argument such as `30`, `5m` or `2h`; a count too large for u64 saturates.
fn sleep_seconds(arg: &str) -> u64 {
    let digits = arg.find(|ch: char| !ch.is_ascii_digit()).unwrap_or(arg.len());
    let scale = match &arg[digits..] {
        "m" => 60,
        "h" => 3_600,
        "d" => 86_400,
        _ => 1,
    };
    match arg[..digits].parse::<u64>() {
        Ok(count) => count.saturating_mul(scale),
        Err(_) if digits > 0 => u64::MAX,
        Err(_) => 0,
    }
}

fn is_number(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_plain_file_reads_and_blind_sleeps_only() {
        let blocked = [
            ("cat package.json", "read(package.json)"),
            ("cd /repo && sed -n '10,40p' src/a.ts", "read(/repo/src/a.ts)"),
            ("sed -n 5p notes.md", "read(notes.md)"),
            ("head -50 README.md", "read(README.md)"),
            ("head -n 20 Cargo.toml", "read(Cargo.toml)"),
            ("sleep 150; gh run list --limit 3", "gh run watch"),
            ("(bun run dev &) ; sleep 30 && curl localhost:3000", "curl -sf --retry"),
            ("sleep 15", "gh run watch"),
            ("sleep 5m && gh run list", "gh run watch"),
            ("sleep 99999999999999999999", "gh run watch"),
            ("gh run rerun 1\nsleep 120\ngh run view 1", "gh run watch"),
            ("(sleep 30) && curl localhost:3000", "curl -sf --retry"),
            ("if true; then sleep 60; fi", "gh run watch"),
            ("sleep 60 && echo waiting for deploy", "gh run watch"),
            ("sleep 60; for f in *.log; do wc -l $f; done", "gh run watch"),
            ("head src/main.rs", "read(src/main.rs)"),
            ("cat -n src/a.rs", "read(src/a.rs)"),
            ("cat \"notes.md\"", "read(notes.md)"),
            ("head -n20 a.md", "read(a.md)"),
        ];
        for (command, advice) in blocked {
            let reason = bash(command).unwrap_or_else(|| panic!("{command} should be blocked"));
            assert!(reason.contains(advice), "{command}: {reason}");
        }
        let allowed = [
            "cat a.log | rg error",
            "cat > /tmp/x.py <<'EOF'\nprint(1)\nEOF",
            "cat src/*.ts",
            "cat a.txt b.txt",
            "tail -50 dev.log",
            "sleep 2 && curl localhost:3000",
            "for i in $(seq 10); do curl -sf localhost && break; sleep 20; done",
            "echo sleep 60",
            "sleep 14",
            "while ! curl -sf localhost; do sleep 20; done",
            "cat --help",
            "cat src/{a,b}.ts",
            "cd $(foo) && cat x",
        ];
        for command in allowed {
            assert_eq!(bash(command), None, "{command}");
        }
    }
}
