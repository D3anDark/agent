use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;

/// Used for a provider with no account or no network; the live list replaces it.
const BUILT_IN: &str = include_str!("models.json");
const REFRESH_AFTER: Duration = Duration::from_secs(24 * 60 * 60);
/// The Codex model list has no output limit; every Codex model takes this many.
const CODEX_MAX_TOKENS: u64 = 128_000;

/// The providers this agent can talk to.
pub const PROVIDERS: [&str; 2] = ["anthropic", "openai-codex"];

#[derive(Clone)]
pub struct Model {
    pub provider: String,
    pub id: String,
    pub name: String,
    pub context_window: u64,
    pub max_tokens: u64,
}

/// The cached catalog while it is under a day old; otherwise asks `fetch` for each provider and caches the result.
/// A provider that cannot be fetched keeps its cached or built-in models.
pub fn load(home: &Path, mut fetch: impl FnMut(&str) -> Result<Vec<Model>, String>) -> Vec<Model> {
    let path = home.join(".agent/models.json");
    let cached = std::fs::read_to_string(&path).ok().and_then(|text| parse(&text));
    let age = std::fs::metadata(&path).and_then(|m| m.modified()).ok().and_then(|modified| modified.elapsed().ok());
    if let Some(cached) = &cached
        && age.is_some_and(|age| age < REFRESH_AFTER)
    {
        return cached.clone();
    }
    let known = cached.unwrap_or_else(built_in);
    let mut catalog = Vec::new();
    let mut fetched_any = false;
    for provider in PROVIDERS {
        match fetch(provider) {
            Ok(models) if models.iter().any(is_small) => {
                fetched_any = true;
                catalog.extend(models);
            }
            _ => catalog.extend(known.iter().filter(|m| m.provider == provider).cloned()),
        }
    }
    if fetched_any {
        // A cache that cannot be written only costs another fetch on the next start.
        std::fs::write(&path, serialize(&catalog)).ok();
    }
    catalog
}

pub fn built_in() -> Vec<Model> {
    parse(BUILT_IN).expect("models.json is valid")
}

/// Anthropic's `GET /v1/models`, newest first.
pub fn parse_anthropic(body: &Value) -> Vec<Model> {
    let mut models = Vec::new();
    for m in body["data"].as_array().into_iter().flatten() {
        let (Some(id), Some(context_window), Some(max_tokens)) = (m["id"].as_str(), m["max_input_tokens"].as_u64(), m["max_tokens"].as_u64()) else { continue };
        let name = m["display_name"].as_str().unwrap_or(id);
        models.push(Model { provider: "anthropic".into(), id: id.into(), name: name.into(), context_window, max_tokens });
    }
    models
}

/// Codex's `GET /codex/models`, in picker order; hidden models are internal.
pub fn parse_codex(body: &Value) -> Vec<Model> {
    let mut models = Vec::new();
    for m in body["models"].as_array().into_iter().flatten() {
        let (Some(id), Some(context_window)) = (m["slug"].as_str(), m["context_window"].as_u64()) else { continue };
        if m["visibility"] != "list" {
            continue;
        }
        let name = m["display_name"].as_str().unwrap_or(id);
        models.push(Model { provider: "openai-codex".into(), id: id.into(), name: name.into(), context_window, max_tokens: CODEX_MAX_TOKENS });
    }
    models
}

/// The provider's newest small model (haiku, luna), used for summaries and permission reviews.
pub fn small_model<'a>(catalog: &'a [Model], provider: &str) -> &'a Model {
    catalog.iter().find(|m| m.provider == provider && is_small(m)).expect("load keeps a small model for every provider")
}

fn is_small(model: &Model) -> bool {
    model.id.contains("haiku") || model.id.contains("luna")
}

fn parse(text: &str) -> Option<Vec<Model>> {
    let entries: Vec<Value> = serde_json::from_str(text).ok()?;
    let mut models = Vec::new();
    for m in &entries {
        models.push(Model {
            provider: m["provider"].as_str()?.into(),
            id: m["id"].as_str()?.into(),
            name: m["name"].as_str()?.into(),
            context_window: m["contextWindow"].as_u64()?,
            max_tokens: m["maxTokens"].as_u64()?,
        });
    }
    Some(models)
}

fn serialize(models: &[Model]) -> String {
    let entries: Vec<Value> = models
        .iter()
        .map(|m| json!({"provider": m.provider, "id": m.id, "name": m.name, "contextWindow": m.context_window, "maxTokens": m.max_tokens}))
        .collect();
    serde_json::to_string_pretty(&entries).expect("models serialize")
}

/// pi's footer label: "Claude Opus 5.5" becomes "opus 5.5".
pub fn short_label(name: &str) -> String {
    let lower = name.to_lowercase();
    lower.strip_prefix("claude ").unwrap_or(&lower).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(provider: &str, id: &str) -> Model {
        Model { provider: provider.into(), id: id.into(), name: id.into(), context_window: 1, max_tokens: 1 }
    }

    #[test]
    fn built_in_catalog_has_a_small_model_per_provider() {
        let models = built_in();
        let opus = models.iter().find(|m| m.id == "claude-opus-5-5").unwrap();
        assert_eq!(short_label(&opus.name), "opus 5.5");
        assert_eq!(small_model(&models, "anthropic").id, "claude-haiku-5-5");
        assert_eq!(small_model(&models, "openai-codex").id, "gpt-6-luna");
    }

    #[test]
    fn load_takes_fetched_lists_and_keeps_built_in_models_for_a_failed_provider() {
        let home = std::env::temp_dir().join(format!("agent-models-{}", std::process::id()));
        std::fs::create_dir_all(home.join(".agent")).unwrap();
        let fetch = |provider: &str| match provider {
            "anthropic" => Ok(vec![model("anthropic", "claude-haiku-9")]),
            _ => Err("No openai-codex account.".to_string()),
        };
        let catalog = load(&home, fetch);
        assert_eq!(catalog[0].id, "claude-haiku-9");
        assert_eq!(catalog.iter().filter(|m| m.provider == "anthropic").count(), 1);
        assert_eq!(small_model(&catalog, "openai-codex").id, "gpt-6-luna");

        let cached = load(&home, |_| panic!("a fresh cache is not refetched"));
        assert_eq!(cached.len(), catalog.len());
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn a_list_without_a_small_model_is_not_trusted() {
        let home = std::env::temp_dir().join(format!("agent-models-small-{}", std::process::id()));
        std::fs::create_dir_all(home.join(".agent")).unwrap();
        let catalog = load(&home, |provider| Ok(vec![model(provider, "big")]));
        assert!(catalog.iter().all(|m| m.id != "big"));
        assert!(!home.join(".agent/models.json").exists());
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn codex_list_skips_hidden_models() {
        let body = json!({"models": [
            {"slug": "gpt-6-luna", "display_name": "GPT-6-Luna", "context_window": 272000, "visibility": "list"},
            {"slug": "codex-auto-review", "context_window": 272000, "visibility": "hide"},
        ]});
        let ids: Vec<String> = parse_codex(&body).into_iter().map(|m| m.id).collect();
        assert_eq!(ids, ["gpt-6-luna"]);
    }
}
