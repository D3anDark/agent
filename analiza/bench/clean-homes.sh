#!/bin/bash
# usage: bench/clean-homes.sh
# Builds the config dirs PROFILE=clean runs use: each harness keeps only its login, no prompts, skills, extensions or hooks.
# pi keeps ~/.pi/agent/SYSTEM.md: with pi's default prompt the subscription login is refused as a third-party app.
set -eu
C=/tmp/bench-clean
rm -rf "$C"; mkdir -p "$C/agent/.agents" "$C/jcode" "$C/pi"

for d in .bun .cargo .config .gitconfig; do ln -s "$HOME/$d" "$C/agent/$d"; done
ln -s "$HOME/.agents/accounts.json" "$C/agent/.agents/accounts.json"
# accounts.default picks the subscription login; without it agent falls back to the workspace API key.
jq '{headroom: false, model: "claude-opus-5-5", thinking: "medium", accounts: {default: .accounts.default}}' \
  "$HOME/.agents/settings.json" > "$C/agent/.agents/settings.json"

for f in auth.json auth-refresh-state.json; do ln -s "$HOME/.jcode/$f" "$C/jcode/$f"; done

ln -s "$HOME/.pi/agent/auth.json" "$C/pi/auth.json"
cp "$HOME/.pi/agent/SYSTEM.md" "$C/pi/SYSTEM.md"
echo '{"defaultProvider": "anthropic", "defaultModel": "claude-opus-5-5"}' > "$C/pi/settings.json"
