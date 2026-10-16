use std::process::Stdio;

/// Output handling for the model's bash calls, matching pi-rtk-optimizer's defaults.
/// shortcut: pi's build/test/git/linter output filters are not ported; `rtk rewrite` already routes those commands through rtk.
pub struct Rtk {
    enabled: bool,
    /// The bash output tail the model sees; the rest goes to a temp file.
    pub max_bytes: usize,
}

impl Rtk {
    pub const ON: Rtk = Rtk { enabled: true, max_bytes: 12_000 };
    /// For commands the user typed with `!`: only the model's bash calls are rewritten.
    pub const OFF: Rtk = Rtk { enabled: false, max_bytes: super::MAX_BYTES };

    /// The command bash runs: rtk's rewrite of `command`, or `command` itself when rtk has none or is not installed.
    pub fn rewrite(&self, command: &str) -> String {
        let trimmed = command.trim_start();
        if !self.enabled || trimmed.is_empty() || trimmed == "rtk" || trimmed.starts_with("rtk ") {
            return command.to_string();
        }
        let output = crate::process::command("rtk").args(["rewrite", command]).stderr(Stdio::null()).output();
        match output {
            Ok(output) => rewritten(command, output.status.code(), &String::from_utf8_lossy(&output.stdout)).unwrap_or_else(|| command.to_string()),
            Err(_) => command.to_string(),
        }
    }

    /// The bash output without ANSI codes, so they spend none of `max_bytes`.
    pub fn strip(&self, output: String) -> String {
        if self.enabled { strip_ansi(&output) } else { output }
    }
}

/// `rtk rewrite` exits 0 or 3 with the new command; 1 means no rtk equivalent, 2 means rtk refuses.
fn rewritten(original: &str, exit_code: Option<i32>, stdout: &str) -> Option<String> {
    if !matches!(exit_code, Some(0 | 3)) {
        return None;
    }
    let command = stdout.trim();
    (!command.is_empty() && command != original).then(|| command.to_string())
}

/// Removes CSI (`ESC [ ... letter`) and OSC (`ESC ] ... BEL` or `ESC ] ... ESC \`) sequences.
pub fn strip_ansi(text: &str) -> String {
    if !text.contains('\x1b') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\x1b' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('[') => {
                while chars.next().is_some_and(|c| !c.is_ascii_alphabetic()) {}
            }
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\x07' || (c == '\x1b' && chars.next_if_eq(&'\\').is_some()) {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrite_needs_success_and_a_different_command() {
        assert_eq!(rewritten("git status", Some(3), "rtk git status\n").as_deref(), Some("rtk git status"));
        assert_eq!(rewritten("git log", Some(0), "rtk git log").as_deref(), Some("rtk git log"));
        assert_eq!(rewritten("echo hi", Some(1), ""), None);
        assert_eq!(rewritten("rm x", Some(2), "rtk rm x"), None);
        assert_eq!(rewritten("ls", Some(0), "ls\n"), None);
        assert_eq!(rewritten("ls", Some(0), "  "), None);
    }

    #[test]
    fn output_loses_ansi_only_when_enabled() {
        assert_eq!(Rtk::ON.strip("\x1b[31mred\x1b[0m \x1b]8;;http://x\x07link\x1b]8;;\x1b\\".into()), "red link");
        assert_eq!(Rtk::OFF.strip("\x1b[1mx".into()), "\x1b[1mx");
    }
}
