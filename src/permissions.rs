use serde_json::Value;
use std::collections::HashSet;
use std::path::Path;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Mode {
    FullAccess,
    Ask,
    Auto,
}

pub const MODES: [Mode; 3] = [Mode::Ask, Mode::Auto, Mode::FullAccess];

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Mode::FullAccess => "full access",
            Mode::Ask => "ask",
            Mode::Auto => "auto",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Mode::FullAccess => "run every tool without asking",
            Mode::Ask => "ask before commands and file edits",
            Mode::Auto => "a reviewer model approves or denies commands and file edits",
        }
    }

    pub fn parse(text: &str) -> Option<Mode> {
        match text {
            "full" | "full access" | "full-access" | "yolo" => Some(Mode::FullAccess),
            "ask" => Some(Mode::Ask),
            "auto" => Some(Mode::Auto),
            _ => None,
        }
    }
}

pub enum Decision {
    Allow,
    /// Allow now and for the rest of the session (same command, or same file for edits).
    AllowAlways,
    Deny,
}

/// What the user or the reviewer is asked to approve.
#[derive(Clone)]
pub struct Request {
    /// "Command" or "Edit", the right-hand label in fx's prompt.
    pub kind: &'static str,
    pub question: &'static str,
    /// The command, or the tool and path.
    pub detail: String,
    /// The key "don't ask again" remembers: the exact command, or the edited path.
    pub grant: String,
}

/// None for tools that only read. `edited_file` is the file an anchored edit targets.
pub fn request_for(tool: &str, input: &Value, edited_file: Option<&Path>) -> Option<Request> {
    let field = |key: &str| input[key].as_str().unwrap_or_default().to_string();
    match tool {
        "bash" | "bg_start" => Some(Request { kind: "Command", question: "Would you like to run the following command?", detail: field("command"), grant: format!("bash:{}", field("command")) }),
        "write" => Some(Request { kind: "Edit", question: "Would you like to write the following file?", detail: field("path"), grant: format!("edit:{}", field("path")) }),
        "undo_last_change" => Some(Request { kind: "Edit", question: "Would you like to undo the last change to this file?", detail: field("path"), grant: format!("edit:{}", field("path")) }),
        "replace" | "replace_match" | "insert" | "copy" | "move" => {
            let path = edited_file.map_or("unknown file".to_string(), |p| p.display().to_string());
            Some(Request { kind: "Edit", question: "Would you like to make the following edit?", detail: format!("{tool} in {path}"), grant: format!("edit:{path}") })
        }
        _ if tool.starts_with("mcp__") => Some(Request { kind: "MCP", question: "Would you like to call the following MCP tool?", detail: format!("{tool} {input}"), grant: format!("mcp:{tool}") }),
        _ if tool.contains("__") => Some(Request { kind: "Plugin", question: "Would you like to call the following plugin tool?", detail: format!("{tool} {input}"), grant: format!("plugin:{tool}") }),
        _ => None,
    }
}

/// Session grants from "don't ask again".
#[derive(Default)]
pub struct Grants(HashSet<String>);

impl Grants {
    pub fn allows(&self, request: &Request) -> bool {
        self.0.contains(&request.grant)
    }

    pub fn add(&mut self, request: &Request) {
        self.0.insert(request.grant.clone());
    }
}

pub const REVIEW_SYSTEM: &str = "You review tool calls a coding agent wants to run on the user's machine. \
Allow anything a careful developer would run without asking while working on the user's request in the working directory: builds, tests, file edits, reading, git status and diff. \
Deny calls that destroy data outside the task, touch secrets, push or publish, change system settings, or do not follow from the request. \
Answer with ALLOW or DENY on the first line, then one short sentence of reason.";

pub fn review_prompt(user_request: &str, cwd: &Path, request: &Request) -> String {
    format!("Working directory: {}\nUser request: {user_request}\n\nTool call ({}):\n{}", cwd.display(), request.kind, request.detail)
}

/// Reads the reviewer's answer; anything but a leading ALLOW denies.
pub fn parse_review(answer: &str) -> (bool, String) {
    let mut lines = answer.trim().lines();
    let verdict = lines.next().unwrap_or_default().trim().to_uppercase();
    let reason = lines.collect::<Vec<_>>().join(" ").trim().to_string();
    (verdict.starts_with("ALLOW"), reason)
}


#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn read_only_tools_need_no_permission() {
        assert!(request_for("read", &json!({"path": "a"}), None).is_none());
        assert!(request_for("rg", &json!({"pattern": "a"}), None).is_none());
        let request = request_for("bash", &json!({"command": "rm -rf x"}), None).unwrap();
        assert_eq!((request.kind, request.detail.as_str()), ("Command", "rm -rf x"));
    }

    #[test]
    fn grants_match_the_exact_command_only() {
        let bash = |command: &str| request_for("bash", &json!({"command": command}), None).unwrap();
        let mut grants = Grants::default();
        grants.add(&bash("cargo test"));
        assert!(grants.allows(&bash("cargo test")));
        assert!(!grants.allows(&bash("cargo test; rm -rf ~")));
    }

    #[test]
    fn review_denies_unless_it_starts_with_allow() {
        assert_eq!(parse_review("ALLOW\nruns tests"), (true, "runs tests".to_string()));
        assert!(!parse_review("I think ALLOW").0);
        assert!(!parse_review("").0);
    }
}
