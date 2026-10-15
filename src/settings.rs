use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::accounts::Policy;
use crate::models::Model;
use crate::permissions::Mode;

/// Everything /settings edits, saved to ~/.agent/settings.json.
#[derive(Clone, PartialEq, Debug)]
pub struct Settings {
    pub model: String,
    pub thinking: String,
    pub permission_mode: Mode,
    pub auto_compact: bool,
    pub reserve_tokens: u64,
    pub keep_recent_tokens: u64,
    pub expand_tools: bool,
    pub condense: bool,
    /// Empty means the default summarizer for the provider.
    pub condense_model: String,
    pub condense_min_batch_chars: u64,
    pub subagent_concurrency: usize,
    /// The effort the swarm prompt tells the model to pass for implementation children.
    pub subagent_effort: String,
    pub switch_at_percent: u64,
    pub when_all_over_limit: Policy,
    /// Empty means the other provider's default model.
    pub fallback_model: String,
    /// Provider to account name, chosen in /account and used first while under the limit.
    pub default_accounts: HashMap<String, String>,
    /// Keyed by provider ("anthropic") or model ("anthropic/claude-opus-5-5"); the model key wins.
    pub context_windows: HashMap<String, u64>,
    /// Skill names left out of the prompt and /skill: completion.
    pub disabled_skills: Vec<String>,
    /// Installed plugins that are not started.
    pub disabled_plugins: Vec<String>,
    /// The user's plugin settings, `{ "<plugin>": { "<key>": value } }`; see plugins/README.md.
    pub plugin_settings: serde_json::Value,
    /// One of `modes::PONYTAIL_LEVELS`.
    pub ponytail: String,
    /// One of `modes::CAVEMAN_LEVELS`.
    pub caveman: String,
    pub rtk: bool,
    /// Send model requests through a local `headroom proxy`.
    pub headroom: bool,
    /// Desktop notification when a task ends or the agent waits for an answer while the terminal is in the background.
    pub notifications: bool,
    /// Transcript lines moved per mouse wheel step.
    pub scroll_lines: usize,
    /// Copy a mouse selection on release; when off, Ctrl+C copies it.
    pub copy_on_select: bool,
}

impl Settings {
    pub fn context_window(&self, model: &Model) -> u64 {
        let overrides = &self.context_windows;
        overrides.get(&model_key(model)).or(overrides.get(&model.provider)).copied().unwrap_or(model.context_window)
    }
}

pub fn model_key(model: &Model) -> String {
    format!("{}/{}", model.provider, model.id)
}

pub fn path(home: &Path) -> PathBuf {
    home.join(".agent/settings.json")
}

pub fn load(home: &Path) -> Settings {
    let own: Value = std::fs::read_to_string(path(home)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    Settings {
        model: own["model"].as_str().unwrap_or("claude-opus-5-5").to_string(),
        thinking: own["thinking"].as_str().unwrap_or("high").to_string(),
        permission_mode: own["permission_mode"].as_str().and_then(Mode::parse).unwrap_or(Mode::Ask),
        auto_compact: own["auto_compact"].as_bool().unwrap_or(true),
        reserve_tokens: own["reserve_tokens"].as_u64().unwrap_or(16_384),
        keep_recent_tokens: own["keep_recent_tokens"].as_u64().unwrap_or(20_000),
        expand_tools: own["expand_tools"].as_bool().unwrap_or(false),
        condense: own["condense"]["enabled"].as_bool().unwrap_or(true),
        condense_model: own["condense"]["model"].as_str().unwrap_or_default().to_string(),
        condense_min_batch_chars: own["condense"]["min_batch_chars"].as_u64().unwrap_or(1000),
        subagent_concurrency: own["subagents"]["concurrency"].as_u64().unwrap_or(4) as usize,
        subagent_effort: own["subagents"]["implementation_effort"].as_str().unwrap_or("medium").to_string(),
        switch_at_percent: own["accounts"]["switch_at_percent"].as_u64().unwrap_or(90),
        when_all_over_limit: own["accounts"]["when_all_over_limit"].as_str().and_then(Policy::parse).unwrap_or(Policy::LeastUsed),
        fallback_model: own["accounts"]["fallback_model"].as_str().unwrap_or_default().to_string(),
        default_accounts: own["accounts"]["default"]
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(provider, name)| Some((provider.clone(), name.as_str()?.to_string())))
            .collect(),
        context_windows: own["context_windows"]
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(key, window)| Some((key.clone(), window.as_u64()?)))
            .collect(),
        disabled_skills: own["disabled_skills"].as_array().into_iter().flatten().filter_map(|name| Some(name.as_str()?.to_string())).collect(),
        disabled_plugins: own["disabled_plugins"].as_array().into_iter().flatten().filter_map(|name| Some(name.as_str()?.to_string())).collect(),
        plugin_settings: if own["plugin_settings"].is_object() { own["plugin_settings"].clone() } else { serde_json::json!({}) },
        ponytail: own["ponytail"].as_str().unwrap_or("full").to_string(),
        caveman: own["caveman"].as_str().unwrap_or("off").to_string(),
        rtk: own["rtk"].as_bool().unwrap_or(true),
        headroom: own["headroom"].as_bool().unwrap_or(false),
        notifications: own["notifications"].as_bool().unwrap_or(true),
        scroll_lines: own["scroll_lines"].as_u64().unwrap_or(3) as usize,
        copy_on_select: own["copy_on_select"].as_bool().unwrap_or(true),
    }
}

pub fn save(home: &Path, settings: &Settings) -> Result<(), String> {
    let path = path(home);
    let value = json!({
        "model": settings.model,
        "thinking": settings.thinking,
        "permission_mode": settings.permission_mode.label(),
        "auto_compact": settings.auto_compact,
        "reserve_tokens": settings.reserve_tokens,
        "keep_recent_tokens": settings.keep_recent_tokens,
        "expand_tools": settings.expand_tools,
        "context_windows": settings.context_windows,
        "disabled_skills": settings.disabled_skills,
        "disabled_plugins": settings.disabled_plugins,
        "plugin_settings": settings.plugin_settings,
        "ponytail": settings.ponytail,
        "caveman": settings.caveman,
        "rtk": settings.rtk,
        "headroom": settings.headroom,
        "notifications": settings.notifications,
        "scroll_lines": settings.scroll_lines,
        "copy_on_select": settings.copy_on_select,
        "condense": {"enabled": settings.condense, "model": settings.condense_model, "min_batch_chars": settings.condense_min_batch_chars},
        "subagents": {"concurrency": settings.subagent_concurrency, "implementation_effort": settings.subagent_effort},
        "accounts": {"switch_at_percent": settings.switch_at_percent, "when_all_over_limit": settings.when_all_over_limit.label(), "fallback_model": settings.fallback_model, "default": settings.default_accounts},
    });
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| format!("Cannot create {}: {e}", path.display()))?;
    std::fs::write(&path, format!("{value:#}\n")).map_err(|e| format!("Cannot write {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_survive_a_save() {
        let home = std::env::temp_dir().join(format!("agent-settings-{}", std::process::id()));
        let mut settings = load(&home);
        assert_eq!((settings.model.as_str(), settings.thinking.as_str(), settings.reserve_tokens), ("claude-opus-5-5", "high", 16_384));
        settings.thinking = "max".into();
        settings.permission_mode = Mode::Auto;
        settings.when_all_over_limit = Policy::OtherProvider;
        settings.context_windows.insert("anthropic".into(), 500_000);
        settings.context_windows.insert("openai-codex/gpt-5.5".into(), 400_000);
        settings.default_accounts.insert("anthropic".into(), "work".into());
        settings.disabled_skills = vec!["remotion-render".into()];
        settings.disabled_plugins = vec!["usagefleet".into()];
        settings.plugin_settings = serde_json::json!({"usagefleet": {"token": "x"}});
        settings.ponytail = "ultra".into();
        settings.caveman = "wenyan".into();
        settings.rtk = false;
        settings.subagent_effort = "low".into();
        settings.headroom = true;
        settings.notifications = false;
        settings.scroll_lines = 10;
        settings.copy_on_select = false;
        save(&home, &settings).unwrap();
        assert_eq!(load(&home), settings);
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn model_window_beats_provider_window_beats_catalog() {
        let catalog = crate::models::built_in();
        let opus = catalog.iter().find(|m| m.provider == "anthropic" && m.id == "claude-opus-4-6").unwrap();
        let sonnet = catalog.iter().find(|m| m.provider == "anthropic" && m.id == "claude-sonnet-4-6").unwrap();
        let mut settings = load(Path::new("/nonexistent"));
        assert_eq!(settings.context_window(opus), opus.context_window);
        settings.context_windows.insert("anthropic".into(), 400_000);
        settings.context_windows.insert("anthropic/claude-opus-4-6".into(), 200_000);
        assert_eq!((settings.context_window(opus), settings.context_window(sonnet)), (200_000, 400_000));
    }
}
