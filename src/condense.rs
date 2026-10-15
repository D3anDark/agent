use serde_json::{Value, json};

use crate::settings::Settings;

/// pi-condense's batch prompt, with tool calls labelled `[[N:name]]` so the summary can cite them.
const SUMMARIZER_SYSTEM: &str = "You are summarizing a batch of tool calls made by an AI coding assistant.
For each tool call provide:
- Tool name and a one-sentence description of what it did
- Key outcome, plus any file paths, identifiers, signatures, or error strings copied verbatim - never reword these
- Any findings the future conversation needs to remember

Keep each tool call to 1-3 bullet points. Skip calls that succeeded with nothing reusable to record. Be concise.

A result containing \" ...[N chars elided]... \" had its middle removed before you saw it: summarize the head and tail you can see, and never report the elided region as empty or absent.

Begin the first bullet of each tool call with that tool's [[N:toolname]] label, copied verbatim (both the number and the name) from its line in the input, as the plain, first thing on the line - no bold, backticks, or list numbering around it. Do not renumber, rename, or invent labels; if you skip a tool, skip its label too.";

const HEAD_CHARS: usize = 4000;
const TAIL_CHARS: usize = 4000;
const PROTECTED_TOOLS: [&str; 4] = ["context_tree_query", "ask_user_question", "todo", "get_goal"];

pub struct Config {
    pub enabled: bool,
    pub model: String,
    pub min_batch_chars: usize,
}

impl Config {
    /// `cheap_model` summarizes unless /settings names a condense model.
    pub fn new(settings: &Settings, cheap_model: &str) -> Config {
        let model = if settings.condense_model.is_empty() { cheap_model } else { &settings.condense_model };
        Config { enabled: settings.condense, model: model.to_string(), min_batch_chars: settings.condense_min_batch_chars as usize }
    }
}

/// One finished tool call of a turn.
pub struct Call {
    pub id: String,
    pub name: String,
    pub input: Value,
    pub output: String,
    pub is_error: bool,
}

/// A summarized batch: short refs (`t3`) for its tool_use ids, and the summary the provider sees instead.
#[derive(Clone, Debug, PartialEq)]
pub struct Condensed {
    pub refs: Vec<(String, String)>,
    pub summary: String,
}

/// The calls worth summarizing: unprotected ones, and only when their outputs reach `min_chars` together.
pub fn select(calls: Vec<Call>, min_chars: usize) -> Vec<Call> {
    let selected: Vec<Call> = calls.into_iter().filter(|call| !is_protected(call)).collect();
    let total: usize = selected.iter().map(|call| call.output.chars().count()).sum();
    if total < min_chars { Vec::new() } else { selected }
}

/// Restored originals and skill files must reach the model verbatim.
fn is_protected(call: &Call) -> bool {
    if PROTECTED_TOOLS.contains(&call.name.as_str()) {
        return true;
    }
    let path = call.input["path"].as_str().unwrap_or_default();
    call.name == "read" && path.contains("/skills/") && path.ends_with(".md")
}

/// The summarizer's system prompt and user prompt for one batch.
pub fn summarizer_prompt(assistant_text: &str, calls: &[Call]) -> (&'static str, String) {
    let mut parts = Vec::new();
    if !assistant_text.is_empty() {
        parts.push(format!("Assistant said: {assistant_text}\n"));
    }
    let tools: Vec<String> = calls
        .iter()
        .enumerate()
        .map(|(index, call)| {
            let status = if call.is_error { "ERROR" } else { "OK" };
            let args = serde_json::to_string_pretty(&call.input).unwrap_or_default();
            format!("[[{}:{}]] Tool: {}({args})\nResult ({status}): {}", index + 1, call.name, call.name, elide(&call.output))
        })
        .collect();
    parts.push(tools.join("\n---\n"));
    (SUMMARIZER_SYSTEM, format!("<tool-call-batch>\n{}\n</tool-call-batch>", parts.join("\n")))
}

fn elide(text: &str) -> String {
    let total = text.chars().count();
    if total <= HEAD_CHARS + TAIL_CHARS {
        return text.to_string();
    }
    let head: String = text.chars().take(HEAD_CHARS).collect();
    let tail: String = text.chars().skip(total - TAIL_CHARS).collect();
    format!("{head} ...[{} chars elided]... {tail}", total - HEAD_CHARS - TAIL_CHARS)
}

/// Numbers the batch's calls from `first_ref` and turns the summary's `[[N:name]]` labels into those refs.
pub fn condensed(summary: &str, calls: &[Call], first_ref: usize) -> Condensed {
    let refs: Vec<(String, String)> = calls.iter().enumerate().map(|(index, call)| (format!("t{}", first_ref + index), call.id.clone())).collect();
    let mut text = String::new();
    let mut rest = summary.trim();
    while let Some(start) = rest.find("[[") {
        text.push_str(&rest[..start]);
        let Some(end) = rest[start..].find("]]") else { break };
        let label = &rest[start + 2..start + end];
        let cited = label.split_once(':').and_then(|(number, name)| {
            let index = number.trim().parse::<usize>().ok()?.checked_sub(1)?;
            (calls.get(index)?.name == name.trim()).then(|| refs[index].0.clone())
        });
        if let Some(reference) = cited {
            text.push_str(&format!("`{reference}`"));
        }
        rest = &rest[start + end + 2..];
    }
    text.push_str(rest);
    Condensed { refs, summary: text }
}

/// What the provider sees: condensed tool results become stubs, their summary joins the same user message.
pub fn view(messages: &[Value], condensed: &[Condensed]) -> Vec<Value> {
    let mut view = messages.to_vec();
    if condensed.is_empty() {
        return view;
    }
    for message in view.iter_mut().filter(|m| m["role"] == "user") {
        let Some(content) = message["content"].as_array_mut() else { continue };
        let mut summaries = Vec::new();
        for block in content.iter_mut().filter(|b| b["type"] == "tool_result") {
            let id = block["tool_use_id"].as_str().unwrap_or_default();
            let Some((index, reference)) = find_ref(condensed, id) else { continue };
            *block = json!({
                "type": "tool_result",
                "tool_use_id": id,
                "content": format!("[Summarized in pruner summary, ref `{reference}`. Use context_tree_query to retrieve full output.]"),
                "is_error": false,
            });
            if !summaries.contains(&index) {
                summaries.push(index);
            }
        }
        for index in summaries {
            content.push(json!({"type": "text", "text": summary_block(&condensed[index])}));
        }
    }
    view
}

fn find_ref<'a>(condensed: &'a [Condensed], id: &str) -> Option<(usize, &'a str)> {
    condensed.iter().enumerate().find_map(|(index, batch)| batch.refs.iter().find(|(_, tool_id)| tool_id == id).map(|(reference, _)| (index, reference.as_str())))
}

fn summary_block(batch: &Condensed) -> String {
    let refs: Vec<String> = batch.refs.iter().map(|(reference, _)| format!("`{reference}`")).collect();
    format!(
        "Pruner summary:\n\n{}\n\n---\n**Summarized tool refs**: {}\nUse `context_tree_query` with these refs to retrieve the original full outputs.",
        batch.summary,
        refs.join(", ")
    )
}

/// context_tree_query: the original outputs for short refs or raw tool_use ids.
pub fn query(messages: &[Value], condensed: &[Condensed], input: &Value) -> Result<String, String> {
    let wanted: Vec<&str> = input["toolCallIds"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    if wanted.is_empty() {
        return Err("[E_BAD_SHAPE] toolCallIds must list at least one ref.".into());
    }
    let mut blocks = Vec::new();
    let mut unknown = Vec::new();
    for reference in wanted {
        let id = condensed.iter().flat_map(|batch| &batch.refs).find(|(short, _)| short == reference).map_or(reference, |(_, id)| id.as_str());
        match original(messages, id) {
            Some((name, args, output, is_error)) => {
                let args = serde_json::to_string_pretty(&args).unwrap_or_default();
                let status = if is_error { "ERROR" } else { "OK" };
                blocks.push(format!("## toolRef: {reference}\nTool: {name}\nArgs: {args}\nStatus: {status}\n\n{}", crate::tools::truncate_head(output, "context")));
            }
            None => unknown.push(reference),
        }
    }
    if !unknown.is_empty() {
        let known: Vec<&str> = condensed.iter().flat_map(|batch| &batch.refs).map(|(short, _)| short.as_str()).collect();
        return Err(format!("Unknown refs: {}. Known refs: {}.", unknown.join(", "), if known.is_empty() { "none".into() } else { known.join(", ") }));
    }
    Ok(blocks.join("\n\n---\n\n"))
}

/// The tool name, input, output and error flag of tool call `id` in the history.
fn original(messages: &[Value], id: &str) -> Option<(String, Value, String, bool)> {
    let blocks = || messages.iter().flat_map(|m| m["content"].as_array().into_iter().flatten());
    let call = blocks().find(|b| b["type"] == "tool_use" && b["id"] == id)?;
    let result = blocks().find(|b| b["type"] == "tool_result" && b["tool_use_id"] == id)?;
    let name = call["name"].as_str().unwrap_or_default().to_string();
    let output = result["content"].as_str().unwrap_or_default().to_string();
    Some((name, call["input"].clone(), output, result["is_error"].as_bool().unwrap_or(false)))
}

pub fn entry(batch: &Condensed) -> Value {
    let refs: serde_json::Map<String, Value> = batch.refs.iter().map(|(short, id)| (short.clone(), json!(id))).collect();
    json!({"type": "condense", "refs": refs, "summary": batch.summary})
}

pub fn from_entry(entry: &Value) -> Condensed {
    let refs = entry["refs"].as_object().into_iter().flatten().map(|(short, id)| (short.clone(), id.as_str().unwrap_or_default().to_string())).collect();
    Condensed { refs, summary: entry["summary"].as_str().unwrap_or_default().to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(id: &str, name: &str, input: Value, output: &str) -> Call {
        Call { id: id.into(), name: name.into(), input, output: output.into(), is_error: false }
    }

    #[test]
    fn selection_skips_small_batches_and_protected_calls() {
        assert!(select(vec![call("a", "bash", json!({}), &"x".repeat(999))], 1000).is_empty());
        let skill = json!({"path": "/u/.pi/agent/skills/tdd/SKILL.md"});
        let picked = select(
            vec![call("a", "read", skill, &"x".repeat(5000)), call("b", "todo", json!({}), &"x".repeat(5000)), call("c", "bash", json!({}), &"x".repeat(1000))],
            1000,
        );
        assert_eq!(picked.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), ["c"]);
    }

    #[test]
    fn labels_become_refs_and_unknown_labels_are_dropped() {
        let calls = [call("a", "bash", json!({}), ""), call("b", "rg", json!({}), "")];
        let batch = condensed("[[1:bash]] ran ls\n[[2:rg]] found x\n[[3:fd]] invented", &calls, 4);
        assert_eq!(batch.summary, "`t4` ran ls\n`t5` found x\n invented");
        assert_eq!(batch.refs, [("t4".to_string(), "a".to_string()), ("t5".to_string(), "b".to_string())]);
    }

    #[test]
    fn view_stubs_condensed_results_and_query_returns_the_original() {
        let messages = [
            json!({"role": "assistant", "content": [{"type": "tool_use", "id": "a", "name": "bash", "input": {"command": "ls"}}]}),
            json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "a", "content": "big output", "is_error": false}]}),
        ];
        let batch = Condensed { refs: vec![("t1".into(), "a".into())], summary: "`t1` listed files".into() };
        let view = view(&messages, std::slice::from_ref(&batch));
        assert_eq!(view[1]["content"][0]["content"], "[Summarized in pruner summary, ref `t1`. Use context_tree_query to retrieve full output.]");
        assert!(view[1]["content"][1]["text"].as_str().unwrap().starts_with("Pruner summary:\n\n`t1` listed files"));
        assert_eq!(messages[1]["content"][0]["content"], "big output");
        assert!(query(&messages, std::slice::from_ref(&batch), &json!({"toolCallIds": ["t1"]})).unwrap().ends_with("Status: OK\n\nbig output"));
        assert_eq!(query(&messages, &[batch], &json!({"toolCallIds": ["t9"]})).unwrap_err(), "Unknown refs: t9. Known refs: t1.");
    }
}
