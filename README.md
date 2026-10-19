# agent

simple af possible.

```sh
curl -fsSL https://agent.rokartur.com/install.sh | sh
```

A coding agent in one Rust binary. No Node. No Electron.

You already pay for Claude or ChatGPT. Log in. That's the setup. Prefer pay as you go? Paste an Anthropic or OpenRouter API key, and every OpenRouter model is in `/model`.

## Features

**Plugins in any language.** A plugin is a folder with `plugin.json` and a program that speaks JSON-RPC over stdin. It can add tools for the model, `/` commands and its own settings. `agent plugin install ./my-plugin`. [Docs](plugins/README.md)

**All your accounts, one agent.** Add several Claude and ChatGPT accounts, Anthropic keys and OpenRouter keys. When one hits its limit, the agent moves to the next. `/usage` shows what is left.

**One folder, nothing else.** Settings, accounts, sessions, skills, plugins, MCP servers and prompts all live in `~/.agents`. A project can add its own `.agents` next to its code. No hidden config anywhere else. Copy the folder and your whole setup moves with it.

**Edits that cannot go stale.** The agent edits by line anchors. If the file changed since it last read it, the edit is refused. No silent overwrites.

**It checks its own work.** After each edit, your language server reports new errors and the checks in `.agents/edit-hooks.json` run. The agent fixes what they find.

**Rewind anything.** Press Esc twice to go back to an earlier prompt. Your files go back with it.

**Subagents.** It splits big work into parallel child agents. You tune how in a plain Markdown prompt, `.agents/swarm-prompt.md`.

**MCP and skills.** Connect MCP servers, OAuth included. Drop in skills and see which ones you never use with `/skill-usage`.

**Web built in.** Search through Exa, Brave, Tavily or Perplexity. Fetch pages as Markdown. Give it a GitHub URL and it clones the repo to read.

**You choose how much it does alone.** Ask before every change, let a reviewer model approve, or `--yolo`.

## Commands worth knowing

```
/goal       keep going until it is done
/btw        ask aside, keep the session clean
/worktree   move the session to a git worktree
/changes    every file it touched, with diffs
/voice      talk instead of type (whisper.cpp)
/caveman    answers without the fluff
/ponytail   writes less code
```

## Scripts and CI

```sh
agent -p "Why is this test flaky?"     # print the answer and exit
agent --headless "Fix the lint"        # JSONL events
```

macOS and Linux. Updates itself. `agent --help` for the rest.
