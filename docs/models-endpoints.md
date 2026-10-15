# Fetching the model catalog from the providers

Question: can `cli/src/models.json` be replaced by asking each provider which models exist, so new models appear without a release?

Checked 2026-10 with live requests. Credentials came from `~/.agent/accounts.json` (Anthropic) and `~/.codex/auth.json` (Codex). They were never printed.

## Anthropic: `GET https://api.anthropic.com/v1/models`

- Works with an API key (`x-api-key`) and with the OAuth token (`authorization: Bearer`, `anthropic-beta: oauth-2025-04-20`). Both returned 200.
- `?limit=100` returns everything in one page (`has_more: false`, 14 models). Pagination uses `has_more` / `last_id`.
- Fields per model: `id, display_name, created_at, line, lifecycle, deprecated_at, retires_at, max_input_tokens, max_tokens, capabilities, type`.
- `max_input_tokens` and `max_tokens` match `contextWindow` and `maxTokens` in `cli/src/models.json` for all 14 ids.
- `cli/src/models.json` also has the aliases `claude-haiku-4-5`, `claude-opus-4-5` and `claude-sonnet-4-5`. The endpoint lists only the dated ids.
- No price. `cost.input` is used only by `models::cheapest()` to pick the summary and review model.

## Codex (ChatGPT subscription): `GET https://chatgpt.com/backend-api/codex/models?client_version=<v>`

- This is the endpoint Codex CLI uses. It caches the response in `~/.codex/models_cache.json` with `fetched_at`, `etag` and `client_version`.
- Auth is the agent's own `openai-codex` account: `authorization: Bearer <access>` and `chatgpt-account-id`. The answer is 200 with a weak `etag`.
- `client_version` is required. Without it the answer is 400. With `0.1.0` the list is empty. With `0.160.0`, `0.200.0` and `9.0.0` the list is the same.
- Fields per model: `slug, display_name, context_window, max_context_window, visibility, priority, supported_in_api, supported_reasoning_levels, input_modalities, ...`.
- `context_window` is 272000, which matches `cli/src/models.json`. `max_context_window` is 872000.
- `visibility` is `list` or `hide`. The live answer had 5 models and 3 of them listed: `gpt-6-luna`, `gpt-5.6-terra`, `gpt-5.6-luna`. `gpt-reserve` and `codex-auto-review` are hidden.
- No price. `priority` looks like picker order.
- The list is authoritative. A one-word request to `/codex/responses` gives `response.completed` for `gpt-6-luna`. For `gpt-6-sol`, `gpt-5.5` and `gpt-5.3-codex-spark` it gives 400 `The '<model>' model is not supported when using Codex with a ChatGPT account.` So the 6 `openai-codex` models in `cli/src/models.json` that are not on the list (`gpt-5.5`, `gpt-5.6-sol`, `gpt-6-sol`, `gpt-6.1-sol`, `gpt-6-astra`, `gpt-5.3-codex-spark`) probably all fail on this account. Only 3 of them were tested.

## Consequences for the design

- Both endpoints give the id, the name and the context window. Both are enough for the picker and for compaction.
- The static catalog is already wrong for Codex. The endpoint is the only source that matches what the account can use.
- Neither endpoint gives a price. `cheapest()` needs another rule, for example a fixed cheap model per provider, the `line` (`haiku`) for Anthropic, or a setting.
- Each request needs a logged-in account for that provider. Without one, the agent still needs a built-in fallback list.
