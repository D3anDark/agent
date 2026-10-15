// pi's audit of 53k bash calls: 10k were cat/sed -n/head file reads (a third of the "edit without read" failures),
// and blind `sleep N` burned 41h of wall time.
const MAX_BLIND_SLEEP: u64 = 15;

/// Why bash should not run `command` (a plain file read or a blind sleep), or None when it may run.
pub fn bash(command: &str) -> Option<String> {
    if let Some(file) = file_read(command) {
        return Some(format!("Use read({file}) with offset/limit instead of bash; edit requires a read."));
    }
    if blind_sleep(command) {
        return Some("No blind sleep. Server: curl -sf --retry 30 --retry-all-errors --retry-delay 1 URL. CI: gh run watch --exit-status, gh pr checks --watch. Long job: load_tools terminals.".into());
    }
    None
}

/// The file of a lone `cat FILE`, `head -N FILE` or `sed -n 'N,Mp' FILE`, optionally after `cd DIR &&`.
fn file_read(command: &str) -> Option<&str> {
    let mut words: Vec<&str> = command.split_whitespace().collect();
    if let ["cd", _, "&&", ..] = words.as_slice() {
        words.drain(..3);
    }
    let file = match words.as_slice() {
        ["cat", file] => file,
        ["head", count, file] if count.strip_prefix("-n").or(count.strip_prefix('-')).is_some_and(is_number) => file,
        ["head", "-n", count, file] if is_number(count) => file,
        ["sed", "-n", range, file] if is_print_range(range) => file,
        _ => return None,
    };
    (!file.contains(['|', '<', '>', ';', '&', '$', '`', '*', '?'])).then_some(file)
}

/// `Np` or `N,Mp`, bare or quoted.
fn is_print_range(range: &str) -> bool {
    let Some(lines) = range.trim_matches(['\'', '"']).strip_suffix('p') else { return false };
    match lines.split_once(',') {
        Some((first, last)) => is_number(first) && is_number(last),
        None => is_number(lines),
    }
}

/// A `sleep N` of 15 s or more that starts a command, outside a polling loop.
fn blind_sleep(command: &str) -> bool {
    let words: Vec<&str> = command.split_whitespace().collect();
    if words.iter().any(|word| matches!(*word, "for" | "while" | "until")) {
        return false;
    }
    words.iter().enumerate().any(|(index, word)| {
        let starts_command = index == 0 || words[index - 1].ends_with([';', '&', '|']);
        let seconds: String = words.get(index + 1).unwrap_or(&"").chars().take_while(char::is_ascii_digit).collect();
        *word == "sleep" && starts_command && seconds.parse().is_ok_and(|seconds: u64| seconds >= MAX_BLIND_SLEEP)
    })
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
            ("cd /repo && sed -n '10,40p' src/a.ts", "read(src/a.ts)"),
            ("sed -n 5p notes.md", "read(notes.md)"),
            ("head -50 README.md", "read(README.md)"),
            ("head -n 20 Cargo.toml", "read(Cargo.toml)"),
            ("sleep 150; gh run list --limit 3", "gh run watch"),
            ("(bun run dev &) ; sleep 30 && curl localhost:3000", "curl -sf --retry"),
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
        ];
        for command in allowed {
            assert_eq!(bash(command), None, "{command}");
        }
    }
}
