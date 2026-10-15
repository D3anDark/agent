use serde_json::Value;

const CATALOG: &str = include_str!("models.json");

/// The providers this agent can talk to; the catalog's other models are hidden.
pub const PROVIDERS: [&str; 2] = ["anthropic", "openai-codex"];

#[derive(Clone)]
pub struct Model {
    pub provider: String,
    pub id: String,
    pub name: String,
    pub context_window: u64,
    pub max_tokens: u64,
    /// Dollars per million input tokens, for picking the cheap summarizer.
    pub input_cost: f64,
}

/// Built-in catalog (copied from pi-ai).
pub fn all() -> Vec<Model> {
    let catalog: Vec<Value> = serde_json::from_str(CATALOG).expect("models.json is valid");
    catalog
        .iter()
        .filter(|m| PROVIDERS.contains(&m["provider"].as_str().unwrap_or_default()))
        .map(|m| {
            let provider = m["provider"].as_str().unwrap_or_default().to_string();
            let id = m["id"].as_str().unwrap_or_default().to_string();
            Model {
                name: m["name"].as_str().unwrap_or(&id).to_string(),
                context_window: m["contextWindow"].as_u64().unwrap_or(200_000),
                max_tokens: m["maxTokens"].as_u64().unwrap_or(32_000),
                input_cost: m["cost"]["input"].as_f64().unwrap_or(f64::MAX),
                provider,
                id,
            }
        })
        .collect()
}

/// The provider's cheapest model, used for summaries and permission reviews.
pub fn cheapest<'a>(catalog: &'a [Model], provider: &str) -> &'a Model {
    let models = catalog.iter().filter(|m| m.provider == provider);
    models.min_by(|a, b| a.input_cost.total_cmp(&b.input_cost)).expect("every provider has catalog models")
}

/// pi's footer label: "Claude Opus 5.5" becomes "opus 5.5".
pub fn short_label(name: &str) -> String {
    let lower = name.to_lowercase();
    lower.strip_prefix("claude ").unwrap_or(&lower).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_has_only_supported_providers_and_a_cheap_model_each() {
        let models = all();
        let opus = models.iter().find(|m| m.provider == "anthropic" && m.id == "claude-opus-5-5").unwrap();
        assert_eq!(short_label(&opus.name), "opus 5.5");
        assert_eq!(cheapest(&models, "anthropic").id, "claude-haiku-5-5");
        assert_eq!(cheapest(&models, "openai-codex").id, "gpt-6-luna");
        assert!(models.iter().all(|m| PROVIDERS.contains(&m.provider.as_str())));
    }
}
