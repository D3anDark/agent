use crossterm::event::{KeyCode, KeyEvent};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::editor::Editor;
use super::panel;
use super::render::{Line, Style, span};
use crate::accounts::{Account, Secret};
use crate::oauth::{self, Pending, Tokens};

const BRIGHT: Style = Style::fg(255).bold();
const CHOICES: [&str; 3] = ["Claude Pro/Max", "Anthropic API key", "ChatGPT Plus/Pro (Codex)"];

/// The /login panel: pick a method, sign in or paste a key, then name the account.
pub enum Login {
    Choose { index: usize },
    /// Anthropic shows `code#state` after sign-in; the user pastes it here.
    ClaudeCode { pending: Pending, input: Editor },
    ApiKey { input: Editor },
    /// A token exchange or the Codex browser callback is running; Esc sets `cancel`.
    Waiting { cancel: Arc<AtomicBool> },
    Name { provider: &'static str, secret: Secret, input: Editor },
}

type Exchange = Box<dyn FnOnce(&AtomicBool) -> Result<Tokens, String> + Send>;

/// What the app does after a key.
pub enum Action {
    None,
    Close,
    /// Shows the sign-in URL the browser was sent to.
    Url(String),
    /// Runs `exchange` off the UI thread with `cancel`, then hands its result to `finished`.
    Exchange { provider: &'static str, url: Option<String>, cancel: Arc<AtomicBool>, exchange: Exchange },
    Save(Account),
}

impl Login {
    pub fn key(&mut self, key: KeyEvent) -> Action {
        let code = key.code;
        if code == KeyCode::Esc {
            if let Login::Waiting { cancel } = self {
                cancel.store(true, Ordering::Relaxed);
            }
            return Action::Close;
        }
        match self {
            Login::Choose { index } => match code {
                KeyCode::Up => *index = index.saturating_sub(1),
                KeyCode::Down => *index = (*index + 1).min(CHOICES.len() - 1),
                KeyCode::Enter => return self.choose(),
                _ => {}
            },
            Login::Waiting { .. } => {}
            Login::ClaudeCode { input, .. } | Login::ApiKey { input } | Login::Name { input, .. } => match code {
                KeyCode::Enter if !input.text().trim().is_empty() => return self.submit(),
                _ => _ = input.edit_key(key),
            },
        }
        Action::None
    }

    pub fn paste(&mut self, text: &str) {
        if let Login::ClaudeCode { input, .. } | Login::ApiKey { input } | Login::Name { input, .. } = self {
            input.insert(text.trim());
        }
    }

    /// An exchange ended. Tokens move on to naming the account; after Esc the result is dropped.
    pub fn finished(&mut self, provider: &'static str, result: Result<Tokens, String>) -> Result<(), String> {
        if !matches!(self, Login::Waiting { .. }) {
            return Ok(());
        }
        let name = if provider == "anthropic" { "claude" } else { "codex" };
        *self = Login::Name { provider, secret: Secret::OAuth(result?), input: Editor::from_text(name) };
        Ok(())
    }

    fn choose(&mut self) -> Action {
        let Login::Choose { index } = self else { return Action::None };
        match *index {
            0 => {
                let pending = oauth::start_claude();
                let url = pending.url.clone();
                oauth::open_browser(&url);
                *self = Login::ClaudeCode { pending, input: Editor::new() };
                Action::Url(url)
            }
            1 => {
                *self = Login::ApiKey { input: Editor::new() };
                Action::None
            }
            _ => {
                let pending = oauth::start_codex();
                let url = pending.url.clone();
                oauth::open_browser(&url);
                let cancel = Arc::new(AtomicBool::new(false));
                *self = Login::Waiting { cancel: cancel.clone() };
                Action::Exchange { provider: "openai-codex", url: Some(url), cancel, exchange: Box::new(move |cancel| oauth::finish_codex(&pending, cancel)) }
            }
        }
    }

    fn submit(&mut self) -> Action {
        let cancel = Arc::new(AtomicBool::new(false));
        match std::mem::replace(self, Login::Waiting { cancel: cancel.clone() }) {
            Login::ClaudeCode { pending, input } => {
                let code = input.text();
                Action::Exchange { provider: "anthropic", url: None, cancel, exchange: Box::new(move |_| oauth::finish_claude(&pending, &code)) }
            }
            Login::ApiKey { input } => {
                *self = Login::Name { provider: "anthropic", secret: Secret::ApiKey(input.text().trim().into()), input: Editor::from_text("api") };
                Action::None
            }
            Login::Name { provider, secret, input } => Action::Save(Account { provider: provider.into(), name: input.text().trim().into(), secret }),
            other => {
                *self = other;
                Action::None
            }
        }
    }

    pub fn render(&self, width: usize, max_rows: usize) -> Vec<Line> {
        let (title, rows, hint) = match self {
            Login::Choose { .. } => {
                let rows = CHOICES.iter().map(|c| panel::Row { left: c.to_string(), right: Vec::new() }).collect();
                ("Login", rows, "↑↓ navigate     enter choose     esc close")
            }
            Login::ClaudeCode { input, .. } => ("Claude login", vec![field("Code", input.field(Some('•')))], "paste the code#state shown after sign-in     enter continue     esc cancel"),
            Login::ApiKey { input } => ("Anthropic API key", vec![field("Key", input.field(Some('•')))], "paste the key     enter continue     esc cancel"),
            Login::Waiting { .. } => ("Login", vec![panel::Row { left: "Waiting for the sign-in to finish...".into(), right: Vec::new() }], "esc cancel"),
            Login::Name { input, .. } => ("Account name", vec![field("Name", input.field(None))], "enter save     esc cancel"),
        };
        let selected = if let Login::Choose { index } = self { *index } else { 0 };
        let view = panel::Panel { title: title.into(), tabs: &[], tab: 0, rows: &rows, selected, empty: "", hint };
        view.render(width, max_rows)
    }
}

fn field(label: &str, value: String) -> panel::Row {
    panel::Row { left: label.into(), right: vec![span(value, BRIGHT)] }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_key_is_masked_then_named_and_saved() {
        let mut login = Login::Choose { index: 0 };
        login.key(KeyCode::Down.into());
        login.key(KeyCode::Enter.into());
        login.paste(" sk-ant-123 ");
        let shown: String = login.render(80, 8).iter().flatten().map(|s| s.text.as_str()).collect();
        assert!(!shown.contains("sk-ant") && shown.contains(&format!("{}\u{E0001}", "•".repeat(10))));
        login.key(KeyCode::Enter.into());
        login.key(KeyCode::Backspace.into());
        login.key(KeyCode::Char('2').into());
        let Action::Save(account) = login.key(KeyCode::Enter.into()) else { panic!("expected a save") };
        assert_eq!((account.provider.as_str(), account.name.as_str()), ("anthropic", "ap2"));
        assert!(matches!(account.secret, Secret::ApiKey(key) if key == "sk-ant-123"));
    }
}
