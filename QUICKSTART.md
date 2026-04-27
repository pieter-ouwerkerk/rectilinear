# Quickstart — Rectilinear + Claude Code

Get from zero to "Claude can file and triage Linear issues for me" in about five minutes.

The full README has architecture, search modes, and all the CLI flags. This file only covers the path most people want: install the binary, point it at your Linear workspace, and wire it into Claude Code.

## 1. Install

```sh
cargo install rectilinear
```

Optional — if you want fully-local embeddings (no Gemini API key needed; adds a ~330MB model and a `cmake` build dependency):

```sh
cargo install rectilinear --features local-embeddings
```

If `cargo install` finishes but `rectilinear` isn't on your `$PATH`, add `~/.cargo/bin` to your shell's `PATH`.

## 2. Connect your Linear workspace

```sh
rectilinear config add-workspace
```

You'll be prompted for:

- **Workspace name** — a short label like `work` or `oss`. Agents pass this around as `workspace`.
- **Linear API key** — get one at https://linear.app/settings/api. The key is org-scoped, so it covers every team in that org.
- **Default team** — the team prefix (e.g. `ENG`, `SFO`) Rectilinear uses when you don't pass `--team`.
- **Set as default workspace?** — `Y` if it's your only/primary workspace.

Config is written to `~/.config/rectilinear/config.toml` with mode `0600`.

## 3. (Recommended) Add a Gemini API key for embeddings

Embeddings are what power semantic search and duplicate detection. Without them, search is FTS-only (still works — just keyword matching).

Get a free key at https://aistudio.google.com/apikey, then:

```sh
rectilinear config set embedding.gemini-api-key AIza...
rectilinear config set embedding.backend api
```

(Or set `GEMINI_API_KEY` in your environment — it overrides the config value.)

Skip this step entirely if you installed with `--features local-embeddings`.

## 4. Sync your team's issues

```sh
rectilinear sync --team ENG --embed
```

First run pulls everything; subsequent syncs are incremental. The `--embed` flag generates embeddings in the same pass, so duplicate detection works immediately.

## 5. Wire it into Claude Code

```sh
claude mcp add rectilinear -s user -- rectilinear serve
```

Verify:

```sh
claude mcp list   # should show: rectilinear: ... ✓ Connected
```

`-s user` means it's available in every project on your machine, not just one repo.

## 6. (Optional) Tell Claude which workspace this repo uses

If you have multiple Linear workspaces, drop a few lines in your project's `CLAUDE.md` (or `AGENTS.md`) so Claude doesn't have to guess:

```markdown
## Linear / Rectilinear

Issues for this repo live in Linear team `ENG`. Use the Rectilinear MCP
with workspace `work` — it's already configured with `ENG` as the default
team, so you don't need to pass `team` explicitly.
```

Single workspace? Skip this — Claude will use the default.

## Now talk to Claude

Things you can just say:

- **"File an issue: Safari users can't reset their password on iOS 17.4."**
  Claude will check for duplicates first via `find_duplicates`, show you any matches, then call `create_issue` if you confirm.

- **"Are there any existing issues about login timeouts on mobile?"**
  Hits `search_issues` with hybrid FTS + vector ranking.

- **"Triage ENG issues."** (or `"Triage some random ENG issues"` for a shuffled batch)
  Claude pulls the triage queue, explores your codebase for each issue using the code search hints, presents one issue at a time with proposed priority/title/description, waits for your input, and applies changes via `mark_triaged`. Run this from inside the relevant project directory so Claude can read the actual code.

- **"Show me ENG-123 and the most similar open issues."**
  Uses `issue_context`.

- **"Add a comment to ENG-456 saying I reproduced this on Safari 17.4."**
  Calls `append_to_issue`.

The MCP server registers its own usage instructions with Claude Code on connect, so you don't need to memorize tool names or argument shapes — natural-language asks work.

## Troubleshooting

- **`rectilinear: command not found`** — add `~/.cargo/bin` to your `PATH`, or use the absolute path `~/.cargo/bin/rectilinear` in the `claude mcp add` command.
- **`claude mcp list` shows `✗ Failed to connect`** — run `rectilinear serve` directly in a terminal; if it errors, the message will say what's missing (usually a config file or a Linear key).
- **Search returns nothing semantic-ish** — you skipped step 3. Either add a Gemini key, or pass `--mode fts` to fall back to keyword search.
- **Triage doesn't find code references** — make sure you're running Claude Code from inside the project directory the issues are about. The triage flow uses your CWD to explore code.
