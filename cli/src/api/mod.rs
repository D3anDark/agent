use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub mod accounts;
pub mod codex;
pub mod models;
pub mod oauth;

use crate::settings::Settings;
use accounts::{Account, Candidate, Pick, Policy, Secret, Usage as Windows};
use models::Model;

const RETRY_DELAYS_SECS: [u64; 4] = [2, 5, 15, 30];
const THINKING_BETA: &str = "thinking-binding-controls-2026-08-01";
/// pi-ai sends OAuth requests as Claude Code: this user agent, its beta flags and its identity as the first system block.
pub const CLAUDE_CODE_USER_AGENT: &str = "claude-cli/2.1.280";
const CLAUDE_CODE_IDENTITY: &str = "You are Claude Code, Anthropic's official CLI for Claude.";
const USAGE_TTL: Duration = Duration::from_secs(60);
/// How long an account that answered 429 is skipped.
const LIMITED_FOR: Duration = Duration::from_secs(60);
/// `headroom proxy`'s default address; it serves both /v1/messages and /backend-api/codex/responses.
const HEADROOM_URL: &str = "http://127.0.0.1:8787";

/// Talks to Anthropic and Codex. Clones share the accounts, the usage cache and the current pick.
#[derive(Clone)]
pub struct Client {
    http: ureq::Agent,
    home: PathBuf,
    anthropic_base: String,
    chatgpt_base: String,
    catalog: Arc<Vec<Model>>,
    /// Codex prompt_cache_key; one per process, like pi's session id.
    cache_key: String,
    state: Arc<Mutex<State>>,
}

#[derive(Default)]
struct State {
    accounts: Vec<Account>,
    usage: HashMap<String, (Instant, Option<Windows>)>,
    limited: HashMap<String, Instant>,
    pinned: HashMap<String, String>,
    current: HashMap<String, String>,
    threshold: f64,
    policy: Option<Policy>,
    fallback_model: String,
    headroom: bool,
}

pub struct Reply {
    pub message: Value,
    pub stop_reason: String,
    pub usage: Usage,
}

pub enum Delta<'a> {
    Text(&'a str),
    Thinking(&'a str),
    /// A retry or an account switch, shown as a system line.
    Notice(&'a str),
}

#[derive(Default, Clone, Copy)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl Usage {
    pub fn add(&mut self, other: Usage) {
        self.input += other.input;
        self.output += other.output;
        self.cache_read += other.cache_read;
        self.cache_write += other.cache_write;
    }

    pub fn context_tokens(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_write
    }
}

/// One row of /account and the footer.
#[derive(Clone)]
pub struct AccountView {
    pub provider: String,
    pub name: String,
    pub oauth: bool,
    pub account_id: Option<String>,
    pub usage: Option<Windows>,
    pub current: bool,
    pub pinned: bool,
}

enum Choice {
    Use(Account),
    Fallback,
}

impl Client {
    /// Loads `~/.agents/accounts.json`; without an Anthropic account, `ANTHROPIC_API_KEY` acts as one named `env`.
    /// Then loads the model catalog, asking each logged-in provider when the cached list is a day old.
    pub fn new(home: &Path) -> Result<Client, String> {
        let base = |var: &str, default: &str| std::env::var(var).unwrap_or_else(|_| default.into()).trim_end_matches('/').to_string();
        let mut client = Client {
            http: ureq::Agent::config_builder().http_status_as_error(false).timeout_connect(Some(Duration::from_secs(30))).build().into(),
            home: home.to_path_buf(),
            anthropic_base: base("ANTHROPIC_BASE_URL", "https://api.anthropic.com"),
            chatgpt_base: base("CHATGPT_BASE_URL", "https://chatgpt.com/backend-api"),
            catalog: Arc::default(),
            cache_key: format!("agent-{}-{}", std::process::id(), crate::now_ms()),
            state: Arc::new(Mutex::new(State { threshold: 90.0, ..State::default() })),
        };
        client.reload(accounts::load(home)?);
        let catalog = models::load(home, |provider| client.list_models(provider));
        client.catalog = Arc::new(catalog);
        Ok(client)
    }

    pub fn catalog(&self) -> &[Model] {
        &self.catalog
    }

    /// The models this provider serves the first saved account right now; see models-endpoints.md.
    fn list_models(&self, provider: &str) -> Result<Vec<Model>, String> {
        let account = self.state.lock().unwrap().accounts.iter().find(|a| a.provider == provider).cloned();
        let account = self.fresh(account.ok_or(format!("No {provider} account."))?)?;
        let http: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).timeout_global(Some(Duration::from_secs(10))).build().into();
        let request = match (&account.secret, provider) {
            (Secret::ApiKey(key), "anthropic") => http
                .get(format!("{}/v1/models?limit=100", self.anthropic_base))
                .header("anthropic-version", "2023-06-01")
                .header("x-api-key", key),
            (Secret::OAuth(tokens), "anthropic") => http
                .get(format!("{}/v1/models?limit=100", self.anthropic_base))
                .header("anthropic-version", "2023-06-01")
                .header("authorization", format!("Bearer {}", tokens.access))
                .header("anthropic-beta", "oauth-2025-04-20"),
            (Secret::OAuth(tokens), _) => http
                // An old client_version gets an empty list, a missing one a 400.
                .get(format!("{}/codex/models?client_version=0.160.0", self.chatgpt_base))
                .header("authorization", format!("Bearer {}", tokens.access))
                .header("chatgpt-account-id", tokens.account_id.as_deref().unwrap_or_default()),
            (Secret::ApiKey(_), _) => return Err(format!("{provider} accounts need /login, not an API key.")),
        };
        let mut response = request.call().map_err(|e| format!("Model list request failed: {e}."))?;
        let status = response.status().as_u16();
        if status != 200 {
            return Err(format!("Model list request failed ({status})."));
        }
        let text = response.body_mut().read_to_string().map_err(|e| format!("Model list request failed: {e}."))?;
        let body: Value = serde_json::from_str(&text).map_err(|e| format!("Model list is not JSON: {e}."))?;
        Ok(match provider {
            "anthropic" => models::parse_anthropic(&body),
            _ => models::parse_codex(&body),
        })
    }

    pub fn configure(&self, settings: &Settings) {
        let mut state = self.state.lock().unwrap();
        state.threshold = settings.switch_at_percent as f64;
        state.policy = Some(settings.when_all_over_limit);
        state.fallback_model = settings.fallback_model.clone();
        let state = &mut *state;
        // A new pin from /account wins over the account the last request used.
        state.current.retain(|provider, _| state.pinned.get(provider) == settings.default_accounts.get(provider));
        state.pinned = settings.default_accounts.clone();
        state.headroom = settings.headroom && crate::installed("headroom");
    }

    fn reload(&self, mut saved: Vec<Account>) {
        let env_key = std::env::var("ANTHROPIC_API_KEY").ok().filter(|k| !k.is_empty());
        if let Some(key) = env_key.filter(|_| !saved.iter().any(|a| a.provider == "anthropic")) {
            saved.push(Account { provider: "anthropic".into(), name: "env".into(), secret: Secret::ApiKey(key) });
        }
        self.state.lock().unwrap().accounts = saved;
    }

    /// Streams one model turn into `on_delta` on a fitting account. Setting `abort` ends the stream with stop reason "aborted".
    pub fn send(&self, request: &Value, abort: &AtomicBool, on_delta: &mut dyn FnMut(Delta)) -> Result<Reply, String> {
        let mut request = request.clone();
        let mut fell_back = false;
        let mut rate_limited: Vec<String> = Vec::new();
        let mut attempt = 0;
        loop {
            let provider = self.provider_of(&request)?;
            let account = match self.choose(&provider, fell_back, on_delta)? {
                Choice::Use(account) => self.fresh(account)?,
                Choice::Fallback => {
                    request = self.fallback(request, &provider, on_delta)?;
                    fell_back = true;
                    continue;
                }
            };
            let error = match self.try_send(&account, &request, abort, on_delta) {
                Ok(reply) => return Ok(reply),
                Err(error) => error,
            };
            let key = key(&account);
            // A quota error moves the same request to the next account once; a second one waits like any retry.
            if error.quota && !rate_limited.contains(&key) {
                self.state.lock().unwrap().limited.insert(key.clone(), Instant::now() + LIMITED_FOR);
                rate_limited.push(key);
                continue;
            }
            if !error.retryable || attempt == RETRY_DELAYS_SECS.len() || abort.load(Ordering::Relaxed) {
                return Err(error.message);
            }
            on_delta(Delta::Notice(&format!("{} Retrying in {}s.", error.message, RETRY_DELAYS_SECS[attempt])));
            std::thread::sleep(Duration::from_secs(RETRY_DELAYS_SECS[attempt]));
            attempt += 1;
        }
    }

    fn provider_of(&self, request: &Value) -> Result<String, String> {
        let id = request["model"].as_str().unwrap_or_default();
        let model = self.catalog.iter().find(|m| m.id == id).ok_or(format!("Unknown model {id}."))?;
        Ok(model.provider.clone())
    }

    /// Picks the account for this request and reports a change of account as "Switched to <name>: <reason>".
    fn choose(&self, provider: &str, fell_back: bool, on_delta: &mut dyn FnMut(Delta)) -> Result<Choice, String> {
        let accounts: Vec<Account> = self.state.lock().unwrap().accounts.iter().filter(|a| a.provider == provider).cloned().collect();
        if accounts.is_empty() {
            let hint = if provider == "anthropic" { " or set ANTHROPIC_API_KEY" } else { "" };
            return Err(format!("No {provider} account. Use /login{hint}."));
        }
        self.refresh_usage(&accounts);
        let mut state = self.state.lock().unwrap();
        let now = Instant::now();
        let candidates: Vec<Candidate> = accounts
            .iter()
            .map(|a| Candidate { usage: state.usage.get(&key(a)).and_then(|(_, u)| u.clone()), limited: state.limited.get(&key(a)).is_some_and(|until| *until > now) })
            .collect();
        let pinned = state.pinned.get(provider).and_then(|name| accounts.iter().position(|a| &a.name == name));
        let policy = state.policy.unwrap_or(Policy::LeastUsed);
        // After falling back once, the other provider's accounts are the last resort, so least-used decides.
        let policy = if fell_back && policy == Policy::OtherProvider { Policy::LeastUsed } else { policy };
        let threshold = state.threshold;
        let (index, over_limit) = match accounts::pick(&candidates, threshold, policy, pinned) {
            Pick::Account(index) => (index, false),
            Pick::OverLimit(index) => (index, true),
            Pick::OtherProvider => return Ok(Choice::Fallback),
            Pick::Blocked { resets_at } => {
                let reset = resets_at.map_or(String::new(), |at| format!(" The first window resets {}.", accounts::resets_in(at)));
                return Err(format!("Every {provider} account is over {threshold}% of its limit.{reset}"));
            }
        };
        let chosen = &accounts[index];
        let previous = state.current.insert(provider.to_string(), chosen.name.clone());
        let Some(previous) = previous.filter(|p| *p != chosen.name) else { return Ok(Choice::Use(chosen.clone())) };
        let previous_index = accounts.iter().position(|a| a.name == previous);
        let reason = match previous_index.map(|i| &candidates[i]) {
            Some(c) if c.limited => format!("{previous} hit a rate limit"),
            Some(Candidate { usage: Some(u), .. }) if u.peak() >= threshold => format!("{previous} is at {:.0}%", u.peak()),
            _ if Some(index) == pinned => "picked in /account".into(),
            None => format!("{previous} is gone"),
            Some(_) => "it comes first".into(),
        };
        let reason = if over_limit { format!("{reason}; every account is over {threshold}%, this one has the most room") } else { reason };
        on_delta(Delta::Notice(&format!("Switched to {}: {reason}", chosen.name)));
        Ok(Choice::Use(chosen.clone()))
    }

    /// Fetches windows older than a minute for OAuth accounts; a failed fetch counts as unknown, which does not block.
    fn refresh_usage(&self, accounts: &[Account]) {
        for account in accounts.iter().filter(|a| a.is_oauth()) {
            let fresh = self.state.lock().unwrap().usage.get(&key(account)).is_some_and(|(at, _)| at.elapsed() < USAGE_TTL);
            if fresh {
                continue;
            }
            let base = if account.provider == "anthropic" { &self.anthropic_base } else { &self.chatgpt_base };
            let usage = self.fresh(account.clone()).ok().and_then(|account| accounts::fetch_usage(&account, base).ok());
            self.state.lock().unwrap().usage.insert(key(account), (Instant::now(), usage));
        }
    }

    /// The account with tokens valid for five more minutes, refreshing and saving them when needed.
    fn fresh(&self, account: Account) -> Result<Account, String> {
        let Secret::OAuth(tokens) = &account.secret else { return Ok(account) };
        let valid = |t: &oauth::Tokens| t.expires > crate::now_ms() + oauth::REFRESH_MARGIN_MS;
        if valid(tokens) {
            return Ok(account);
        }
        // Another agent process may have refreshed already; its refresh token replaced ours.
        let saved = accounts::load(&self.home)?.into_iter().find(|a| a.provider == account.provider && a.name == account.name);
        if let Some(saved) = saved.filter(|a| matches!(&a.secret, Secret::OAuth(t) if valid(t))) {
            self.reload(accounts::load(&self.home)?);
            return Ok(saved);
        }
        let refreshed = match account.provider.as_str() {
            "anthropic" => oauth::refresh_claude(&tokens.refresh),
            _ => oauth::refresh_codex(&tokens.refresh),
        }
        .map_err(|e| format!("Cannot refresh {}: {e} Use /login again.", account.name))?;
        let refreshed = oauth::Tokens { account_id: refreshed.account_id.or_else(|| tokens.account_id.clone()), ..refreshed };
        let account = Account { secret: Secret::OAuth(refreshed), ..account };
        let updated = account.clone();
        let saved = accounts::update(&self.home, |list| {
            match list.iter_mut().find(|a| a.provider == updated.provider && a.name == updated.name) {
                Some(slot) => *slot = updated,
                None => list.push(updated),
            }
            Ok(())
        })?;
        self.reload(saved);
        Ok(account)
    }

    /// `other-provider`: the same request on `accounts.fallback_model`, or the other provider's default.
    fn fallback(&self, mut request: Value, provider: &str, on_delta: &mut dyn FnMut(Delta)) -> Result<Value, String> {
        let configured = self.state.lock().unwrap().fallback_model.clone();
        let model = match configured.as_str() {
            "" if provider == "anthropic" => self.catalog.iter().find(|m| m.provider == "openai-codex"),
            "" => self.catalog.iter().find(|m| m.id == "claude-opus-5-5"),
            id => self.catalog.iter().find(|m| m.id == id),
        };
        let model = model.filter(|m| m.provider != provider).ok_or(format!("Every {provider} account is over its limit and accounts.fallback_model names no other provider's model."))?;
        on_delta(Delta::Notice(&format!("Switched to {}: every {provider} account is over its limit", model.id)));
        request["model"] = json!(model.id);
        request["max_tokens"] = json!(request["max_tokens"].as_u64().unwrap_or(model.max_tokens).min(model.max_tokens));
        Ok(request)
    }

    fn try_send(&self, account: &Account, request: &Value, abort: &AtomicBool, on_delta: &mut dyn FnMut(Delta)) -> Result<Reply, ApiError> {
        let headroom = self.state.lock().unwrap().headroom;
        let anthropic_base = if headroom { HEADROOM_URL } else { &self.anthropic_base };
        let chatgpt_base = if headroom { &format!("{HEADROOM_URL}/backend-api") } else { &self.chatgpt_base };
        let (call, body) = match (account.provider.as_str(), &account.secret) {
            ("anthropic", secret) => {
                let call = self.http.post(format!("{anthropic_base}/v1/messages")).header("anthropic-version", "2023-06-01");
                match secret {
                    Secret::ApiKey(key) => (call.header("x-api-key", key).header("anthropic-beta", THINKING_BETA), request.clone()),
                    Secret::OAuth(tokens) => {
                        let mut body = request.clone();
                        let mut system = vec![json!({"type": "text", "text": CLAUDE_CODE_IDENTITY})];
                        system.extend(body["system"].as_array().cloned().unwrap_or_default());
                        body["system"] = json!(system);
                        let call = call
                            .header("authorization", format!("Bearer {}", tokens.access))
                            .header("anthropic-beta", format!("claude-code-20250219,oauth-2025-04-20,{THINKING_BETA}"))
                            .header("user-agent", CLAUDE_CODE_USER_AGENT)
                            .header("x-app", "cli")
                            .header("anthropic-dangerous-direct-browser-access", "true");
                        (call, body)
                    }
                }
            }
            (_, Secret::OAuth(tokens)) => {
                let call = self
                    .http
                    .post(format!("{chatgpt_base}/codex/responses"))
                    .header("authorization", format!("Bearer {}", tokens.access))
                    .header("chatgpt-account-id", tokens.account_id.as_deref().unwrap_or_default())
                    .header("OpenAI-Beta", "responses=experimental")
                    .header("originator", "pi")
                    .header("accept", "text/event-stream")
                    .header("session-id", &self.cache_key);
                (call, codex::request(request, &self.cache_key))
            }
            (provider, Secret::ApiKey(_)) => return Err(ApiError::fatal(format!("{provider} accounts need /login, not an API key."))),
        };
        let hint = if headroom { format!(" Is `headroom proxy` running on {HEADROOM_URL}?") } else { String::new() };
        let response = call.header("content-type", "application/json").send(body.to_string()).map_err(|e| ApiError::retryable(format!("Request failed: {e}.{hint}")))?;
        let status = response.status().as_u16();
        let mut reader = BufReader::new(response.into_body().into_reader());
        if status != 200 {
            let mut text = String::new();
            let _ = std::io::Read::read_to_string(&mut reader, &mut text);
            let quota = status == 429 || (status == 529 && text.contains("quota"));
            let retryable = status == 408 || status == 429 || status >= 500;
            return Err(ApiError { message: format!("API error {status}: {}", text.trim()), retryable, quota });
        }
        if account.provider == "anthropic" { parse_stream(&mut reader, abort, on_delta) } else { codex::parse_stream(&mut reader, abort, on_delta) }
    }

    pub fn accounts(&self) -> Vec<AccountView> {
        let state = self.state.lock().unwrap();
        let mut views: Vec<AccountView> = state
            .accounts
            .iter()
            .map(|a| AccountView {
                provider: a.provider.clone(),
                name: a.name.clone(),
                oauth: a.is_oauth(),
                account_id: a.account_id().map(String::from),
                usage: state.usage.get(&key(a)).and_then(|(_, u)| u.clone()),
                current: state.current.get(&a.provider) == Some(&a.name),
                pinned: state.pinned.get(&a.provider) == Some(&a.name),
            })
            .collect();
        views.sort_by(|a, b| (!a.oauth, &a.provider, &a.name).cmp(&(!b.oauth, &b.provider, &b.name)));
        views
    }

    /// Fetches every OAuth account's windows that are older than a minute, for /account.
    pub fn refresh_all_usage(&self) {
        let accounts = self.state.lock().unwrap().accounts.clone();
        self.refresh_usage(&accounts);
    }

    /// Saves an account, replacing one with the same provider and name.
    pub fn add(&self, account: Account) -> Result<(), String> {
        let saved = accounts::update(&self.home, |list| {
            list.retain(|a| a.provider != account.provider || a.name != account.name);
            list.push(account);
            Ok(())
        })?;
        self.reload(saved);
        Ok(())
    }

    pub fn remove(&self, provider: &str, name: &str) -> Result<(), String> {
        let saved = accounts::update(&self.home, |list| {
            let before = list.len();
            list.retain(|a| a.provider != provider || a.name != name);
            if list.len() == before { Err(format!("{name} is not a saved account.")) } else { Ok(()) }
        })?;
        self.reload(saved);
        Ok(())
    }

    /// Name and provider account id of the account the next request is likely to use: the current pick, the pinned one, or the first.
    pub fn account(&self, provider: &str) -> Option<(String, Option<String>)> {
        let state = self.state.lock().unwrap();
        let name = state.current.get(provider).or_else(|| state.pinned.get(provider));
        let mut candidates = state.accounts.iter().filter(|a| a.provider == provider);
        let account = match name {
            Some(name) => candidates.find(|a| &a.name == name),
            None => candidates.next(),
        }?;
        Some((account.name.clone(), account.account_id().map(String::from)))
    }
}

fn key(account: &Account) -> String {
    format!("{}/{}", account.provider, account.name)
}

pub struct ApiError {
    pub message: String,
    pub retryable: bool,
    /// 429, or 529 that names a quota: the account is out, another one may not be.
    pub quota: bool,
}

impl ApiError {
    pub fn retryable(message: String) -> ApiError {
        ApiError { message, retryable: true, quota: false }
    }

    pub fn fatal(message: String) -> ApiError {
        ApiError { message, retryable: false, quota: false }
    }
}

/// Builds a Messages request with cache breakpoints on the system prompt, the last tool, and the last message.
/// `thinking` is pi's level; "off" sends no thinking config and "minimal" maps to the lowest effort.
pub fn request(model: &str, max_tokens: u64, thinking: &str, system: &str, tools: &Value, messages: &[Value]) -> Value {
    let mut tools = tools.clone();
    if let Some(last) = tools.as_array_mut().and_then(|t| t.last_mut()) {
        last["cache_control"] = json!({"type": "ephemeral"});
    }
    let mut messages = messages.to_vec();
    if let Some(block) = messages.last_mut().and_then(|m| m["content"].as_array_mut()).and_then(|c| c.last_mut()) {
        block["cache_control"] = json!({"type": "ephemeral"});
    }
    let mut request = json!({
        "model": model,
        "max_tokens": max_tokens,
        "stream": true,
        "system": [{"type": "text", "text": system, "cache_control": {"type": "ephemeral"}}],
        "tools": tools,
        "messages": messages,
    });
    if thinking != "off" {
        let effort = if thinking == "minimal" { "low" } else { thinking };
        // Same as pi: thinking that no longer matches the history after compaction is dropped instead of failing with 400.
        request["thinking"] = json!({"type": "adaptive", "display": "summarized", "block_binding": {"prefix_mismatch_behavior": "drop_block"}});
        request["output_config"] = json!({"effort": effort});
    }
    request
}

/// A one-shot request without tools or thinking, used for compaction summaries.
pub fn plain_request(model: &str, system: &str, prompt: &str, max_tokens: u64) -> Value {
    json!({
        "model": model,
        "max_tokens": max_tokens,
        "stream": true,
        "system": [{"type": "text", "text": system}],
        "messages": [{"role": "user", "content": [{"type": "text", "text": prompt}]}],
    })
}

fn parse_stream(reader: &mut dyn BufRead, abort: &AtomicBool, on_delta: &mut dyn FnMut(Delta)) -> Result<Reply, ApiError> {
    let mut blocks: Vec<Value> = Vec::new();
    let mut tool_json: Vec<String> = Vec::new();
    let mut usage = Usage::default();
    let mut stop_reason = String::new();
    let mut line = String::new();
    let cut_off = |what: &str| ApiError::retryable(format!("Stream ended {what}."));
    loop {
        if abort.load(Ordering::Relaxed) {
            stop_reason = "aborted".into();
            break;
        }
        line.clear();
        let read = reader.read_line(&mut line).map_err(|e| ApiError::retryable(format!("Stream read failed: {e}.")))?;
        if read == 0 {
            return Err(cut_off("before message_stop"));
        }
        let Some(data) = line.strip_prefix("data:") else { continue };
        let event: Value = serde_json::from_str(data.trim()).map_err(|e| ApiError::fatal(format!("Bad stream event: {e}.")))?;
        match event["type"].as_str().unwrap_or_default() {
            "message_start" => read_usage(&event["message"]["usage"], &mut usage),
            "content_block_start" => {
                blocks.push(event["content_block"].clone());
                tool_json.push(String::new());
            }
            "content_block_delta" => {
                let index = event["index"].as_u64().unwrap_or(0) as usize;
                let (Some(block), Some(partial)) = (blocks.get_mut(index), tool_json.get_mut(index)) else {
                    return Err(cut_off("with a delta for an unknown block"));
                };
                let delta = &event["delta"];
                match delta["type"].as_str().unwrap_or_default() {
                    "text_delta" => {
                        let text = delta["text"].as_str().unwrap_or_default();
                        on_delta(Delta::Text(text));
                        append(&mut block["text"], text);
                    }
                    "thinking_delta" => {
                        let text = delta["thinking"].as_str().unwrap_or_default();
                        on_delta(Delta::Thinking(text));
                        append(&mut block["thinking"], text);
                    }
                    "signature_delta" => append(&mut block["signature"], delta["signature"].as_str().unwrap_or_default()),
                    "input_json_delta" => partial.push_str(delta["partial_json"].as_str().unwrap_or_default()),
                    _ => {}
                }
            }
            "content_block_stop" => {
                let index = event["index"].as_u64().unwrap_or(0) as usize;
                if let (Some(block), Some(partial)) = (blocks.get_mut(index), tool_json.get(index))
                    && block["type"] == "tool_use"
                    && !partial.is_empty()
                {
                    block["input"] = serde_json::from_str(partial).map_err(|e| ApiError::fatal(format!("Bad tool input JSON: {e}.")))?;
                }
            }
            "message_delta" => {
                stop_reason = event["delta"]["stop_reason"].as_str().unwrap_or_default().to_string();
                read_usage(&event["usage"], &mut usage);
            }
            "message_stop" => break,
            "error" => {
                let kind = event["error"]["type"].as_str().unwrap_or_default();
                let retryable = matches!(kind, "overloaded_error" | "api_error" | "rate_limit_error");
                let quota = kind == "rate_limit_error";
                return Err(ApiError { message: format!("API stream error: {}", event["error"]), retryable, quota });
            }
            _ => {}
        }
    }
    Ok(Reply { message: json!({"role": "assistant", "content": blocks}), stop_reason, usage })
}

fn append(field: &mut Value, text: &str) {
    let mut joined = field.as_str().unwrap_or_default().to_string();
    joined.push_str(text);
    *field = Value::String(joined);
}

/// Usage fields arrive in message_start and again (cumulative) in message_delta; later values win.
fn read_usage(raw: &Value, usage: &mut Usage) {
    let fields = [
        ("input_tokens", &mut usage.input),
        ("output_tokens", &mut usage.output),
        ("cache_read_input_tokens", &mut usage.cache_read),
        ("cache_creation_input_tokens", &mut usage.cache_write),
    ];
    for (name, slot) in fields {
        if let Some(value) = raw[name].as_u64() {
            *slot = value;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picking_an_account_reports_it_before_the_next_request() {
        let home = std::env::temp_dir().join(format!("agent-pick-{}", std::process::id()));
        let client = Client::new(&home).unwrap();
        let account = |name: &str| Account { provider: "anthropic".into(), name: name.into(), secret: Secret::ApiKey("sk".into()) };
        client.reload(vec![account("home"), account("work")]);
        client.state.lock().unwrap().current.insert("anthropic".into(), "home".into());

        let mut settings = crate::settings::load(&home);
        settings.default_accounts.insert("anthropic".into(), "work".into());
        client.configure(&settings);

        assert_eq!(client.account("anthropic"), Some(("work".into(), None)));
        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn stream_builds_text_thinking_and_tool_blocks() {
        let events = [
            r#"{"type":"message_start","message":{"usage":{"input_tokens":10,"cache_read_input_tokens":5,"output_tokens":1}}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"hm"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig"}}"#,
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Hi"}}"#,
            r#"{"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"t1","name":"bash","input":{}}}"#,
            r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"command\":"}}"#,
            r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"\"ls\"}"}}"#,
            r#"{"type":"content_block_stop","index":2}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":7}}"#,
            r#"{"type":"message_stop"}"#,
        ];
        let body: String = events.iter().map(|e| format!("event: x\ndata: {e}\n\n")).collect();
        let mut streamed = String::new();
        let on_delta = &mut |delta: Delta| match delta {
            Delta::Text(t) => streamed.push_str(t),
            Delta::Thinking(t) => streamed.push_str(&format!("[{t}]")),
            Delta::Notice(_) => {}
        };
        let reply = parse_stream(&mut body.as_bytes(), &AtomicBool::new(false), on_delta).ok().unwrap();
        assert_eq!(streamed, "[hm]Hi");
        assert_eq!(reply.stop_reason, "tool_use");
        assert_eq!((reply.usage.input, reply.usage.output, reply.usage.cache_read), (10, 7, 5));
        let content = &reply.message["content"];
        assert_eq!(content[0], json!({"type": "thinking", "thinking": "hm", "signature": "sig"}));
        assert_eq!(content[2]["input"], json!({"command": "ls"}));
    }

    #[test]
    fn request_marks_cache_breakpoints() {
        let tools = json!([{"name": "a"}, {"name": "b"}]);
        let messages = [json!({"role": "user", "content": [{"type": "text", "text": "1"}, {"type": "text", "text": "2"}]})];
        let request = request("m", 1000, "high", "sys", &tools, &messages);
        assert!(request["tools"][0].get("cache_control").is_none());
        assert_eq!(request["tools"][1]["cache_control"]["type"], "ephemeral");
        assert!(request["messages"][0]["content"][0].get("cache_control").is_none());
        assert_eq!(request["messages"][0]["content"][1]["cache_control"]["type"], "ephemeral");
        assert_eq!(request["system"][0]["cache_control"]["type"], "ephemeral");
    }
}
