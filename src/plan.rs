use serde_json::{json, Value};

#[derive(Clone, Debug, Default, PartialEq)]
pub enum Plan {
    #[default]
    Off,
    Drafting,
    Ready(String),
}

pub const PROMPT: &str = "<plan_mode>
Plan mode is on. Investigate and design, change nothing. Edits, subagents and state-changing commands are blocked; bash runs only read-only commands (ls, cat, rg, fd, git status/log/diff/show, ...). Ask with ask_user_question when a decision cannot be inferred. When the plan is ready, call plan_mode_complete with the whole plan in Markdown as your last action. The user then picks: implement here, implement in a fresh session, export, or keep planning.
</plan_mode>";

pub const IMPLEMENT: &str = "Implement the plan.";

const ALLOWED_TOOLS: [&str; 15] = [
    "read", "rg", "fd", "bash", "context_tree_query", "ask_user_question", "todo", "get_goal", "load_tools", "bg_status", "bg_list",
    "web_search", "fetch_content", "get_search_content", "plan_mode_complete",
];

const READ_ONLY_COMMANDS: [&str; 18] = ["ls", "cat", "head", "tail", "wc", "rg", "fd", "pwd", "tree", "file", "stat", "which", "echo", "jq", "diff", "du", "cd", "true"];
const READ_ONLY_GIT: [&str; 7] = ["status", "log", "diff", "show", "blame", "ls-files", "rev-parse"];
const RUNS_OTHER_COMMANDS: [&str; 5] = ["-x", "-X", "--exec", "--exec-batch", "--pre"];

pub fn tool() -> Value {
    json!({
        "name": "plan_mode_complete",
        "description": "Submit the finished plan. Call it alone, as your last action in plan mode.",
        "input_schema": {
            "type": "object",
            "properties": { "plan": { "type": "string", "description": "The whole plan in Markdown" } },
            "required": ["plan"]
        }
    })
}

/// Why plan mode blocks this tool call, or None when it may run.
pub fn blocked(tool: &str, input: &Value) -> Option<String> {
    if !ALLOWED_TOOLS.contains(&tool) {
        return Some(format!("Plan mode blocks {tool}: it can change state. Finish the plan with plan_mode_complete."));
    }
    let command = input["command"].as_str().unwrap_or_default();
    if tool == "bash" && !read_only(command) {
        return Some("Plan mode allows only read-only commands (ls, cat, rg, fd, git status/log/diff/show, ...) without redirects or $(...).".into());
    }
    None
}

// shortcut: splits on | ; & and newlines even inside quotes, so `rg 'a|b'` is refused; a shell parser would fix it.
fn read_only(command: &str) -> bool {
    if command.contains(['>', '`']) || command.contains("$(") {
        return false;
    }
    let mut segments = command.split(['|', ';', '&', '\n']).map(str::trim).filter(|segment| !segment.is_empty()).peekable();
    segments.peek().is_some() && segments.all(read_only_segment)
}

fn read_only_segment(segment: &str) -> bool {
    let words: Vec<&str> = segment.split_whitespace().collect();
    if words.iter().any(|word| RUNS_OTHER_COMMANDS.contains(word)) {
        return false;
    }
    match words[0] {
        "git" => words.get(1).is_some_and(|sub| READ_ONLY_GIT.contains(sub)),
        program => READ_ONLY_COMMANDS.contains(&program),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_edits_and_state_changing_commands() {
        assert!(blocked("write", &json!({"path": "a"})).is_some());
        assert!(blocked("subagent", &json!({})).is_some());
        assert!(blocked("read", &json!({"path": "a"})).is_none());
        for command in ["rg -n plan src", "git log --oneline | head -5 && git status", "cd . && ls"] {
            assert!(blocked("bash", &json!({ "command": command })).is_none(), "{command}");
        }
        for command in ["rm -rf x", "cat a > b", "git commit -m x", "fd -x rm", "ls; touch x", "echo $(rm x)", ""] {
            assert!(blocked("bash", &json!({ "command": command })).is_some(), "{command}");
        }
    }
}
