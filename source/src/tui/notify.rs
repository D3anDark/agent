/// The escape sequence that shows a desktop notification in the terminal we run in.
/// `env` reads environment variables; the ones checked survive tmux, which renames TERM_PROGRAM.
pub fn sequence(title: &str, body: &str, env: impl Fn(&str) -> Option<String>) -> String {
    let (title, body) = (printable(title), printable(body));
    let is = |name: &str, value: &str| env(name).is_some_and(|found| found == value);
    let raw = if env("KITTY_WINDOW_ID").is_some() || is("TERM", "xterm-kitty") {
        format!("\x1b]99;;{title}: {body}\x1b\\")
    } else if env("GHOSTTY_RESOURCES_DIR").is_some() || env("WEZTERM_EXECUTABLE").is_some() || env("VTE_VERSION").is_some() || is("TERM", "foot") {
        format!("\x1b]777;notify;{title};{body}\x07")
    } else if is("LC_TERMINAL", "iTerm2") || is("TERM_PROGRAM", "iTerm.app") {
        format!("\x1b]9;{title}: {body}\x07")
    } else {
        // Terminal.app and the rest have no notification sequence; the bell bounces the Dock icon or marks the tab.
        return "\x07".to_string();
    };
    if env("TMUX").is_none() {
        return raw;
    }
    // tmux forwards a sequence to the outer terminal only inside DCS passthrough, with every ESC doubled.
    format!("\x1bPtmux;{}\x1b\\", raw.replace('\x1b', "\x1b\x1b"))
}

/// Control characters would end the sequence early and let the rest of the text drive the terminal.
pub fn printable(text: &str) -> String {
    let line = text.lines().find(|line| !line.trim().is_empty()).unwrap_or_default();
    line.chars().filter(|c| !c.is_control()).take(120).collect::<String>().replace(';', ",")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| vars.iter().find(|(key, _)| *key == name).map(|(_, value)| value.to_string())
    }

    #[test]
    fn each_terminal_family_gets_its_own_sequence() {
        assert_eq!(sequence("agent", "Done", env(&[("GHOSTTY_RESOURCES_DIR", "/g")])), "\x1b]777;notify;agent;Done\x07");
        assert_eq!(sequence("agent", "Done", env(&[("KITTY_WINDOW_ID", "1")])), "\x1b]99;;agent: Done\x1b\\");
        assert_eq!(sequence("agent", "Done", env(&[("TERM_PROGRAM", "iTerm.app")])), "\x1b]9;agent: Done\x07");
        assert_eq!(sequence("agent", "Done", env(&[("TERM_PROGRAM", "Apple_Terminal")])), "\x07");
    }

    #[test]
    fn tmux_wraps_the_sequence_in_passthrough() {
        let vars = env(&[("TMUX", "/tmp/tmux"), ("LC_TERMINAL", "iTerm2")]);
        assert_eq!(sequence("agent", "Done", vars), "\x1bPtmux;\x1b\x1b]9;agent: Done\x07\x1b\\");
    }

    #[test]
    fn text_cannot_inject_escape_sequences() {
        let vars = env(&[("GHOSTTY_RESOURCES_DIR", "/g")]);
        assert_eq!(sequence("agent", "\nrm x\x1b]0;evil\x07; ok\nmore", vars), "\x1b]777;notify;agent;rm x]0,evil, ok\x07");
    }
}
