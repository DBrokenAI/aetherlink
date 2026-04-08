# AetherLink

> Architectural guardrails for AI agents. Written in Rust. MIT licensed.

AetherLink is an MCP server **and** a Claude Code `PreToolUse` hook that
enforces a project's architectural rules at write-time. If an agent
tries to grow a file past your line limit, introduce a forbidden import,
or create a circular dependency, the write is blocked *before* it
touches disk — and the agent gets the rejection reason on stderr so it
can fix and retry.

The hook is the whole point: even if the agent has no idea AetherLink
exists, every `Edit` / `Write` / `MultiEdit` call goes through it. The
agent literally cannot route around it.

## Install

### macOS (Homebrew)

```sh
brew install DBrokenAI/tap/aetherlink
aetherlink --register        # add to Claude Desktop
aetherlink --install-hook    # add to Claude Code
```

### Linux (curl | sh)

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/DBrokenAI/aetherlink/releases/latest/download/aetherlink-installer.sh | sh
aetherlink --register
aetherlink --install-hook
```

### Windows (PowerShell)

```powershell
irm https://github.com/DBrokenAI/aetherlink/releases/latest/download/aetherlink-installer.ps1 | iex
aetherlink --register
aetherlink --install-hook
```

(Or, on Windows, double-click `install.bat` from a release zip — same effect.)

### From source (any OS)

```sh
git clone https://github.com/DBrokenAI/aetherlink
cd aetherlink
cargo build --release
./target/release/aetherlink --register
./target/release/aetherlink --install-hook
```

## The "one rule, one week" onboarding

Don't try to design your whole ruleset upfront. The right way to find
the rules your project actually needs is to start with **one** and
discover the rest over time.

1. Pick the project you know best.
2. Drop a minimal `AetherLink.toml` in the root with one rule:

   ```toml
   [rules]
   forbidden_imports = ["frontend -> backend"]
   ```

3. Run `aetherlink --baseline .` to grandfather any existing
   violations and commit `.aetherlink-baseline.json`.
4. Use the project normally for a week. Don't tell Claude AetherLink
   exists. See what gets blocked.
5. After a week, decide whether your one rule is too loose, too tight,
   or wrong. Add or adjust **one** rule. Re-baseline. Repeat.

The mistake people make is trying to validate the tool with synthetic
tests. Synthetic tests tell you the code runs. Only real usage tells
you the rules are useful.

## Supported rules

```toml
[rules]
# Reject any source file longer than this many lines.
max_file_lines = 500

# Block circular dependencies in the import graph.
no_cycles = true

# Block specific cross-folder imports. Format: "from -> to".
# Each side matches any directory component of a file's path,
# case-insensitively (so `ui -> db` catches `UI/Button.rs` on Linux too).
forbidden_imports = ["ui -> db", "api -> secret"]
```

Per-folder overrides and per-rule severity demotion are also
supported — see `src/rules/config.rs` for the full schema.

## Subcommands

```
aetherlink                Run as an MCP server on stdio (default)
aetherlink --register     Install into Claude Desktop's config
aetherlink --install-hook   Patch ~/.claude/settings.json (Claude Code)
aetherlink --uninstall-hook Remove the AetherLink hook entry
aetherlink --add          Interactively add a rule to AetherLink.toml
aetherlink --tray         Run the system tray health indicator
aetherlink --baseline [PATH]
                          Snapshot current violations into
                          .aetherlink-baseline.json
aetherlink --version
aetherlink --help
```

## Bypass

Create an empty file named `.aetherlink_bypass` in the project root.
AetherLink will allow writes through (with a CRITICAL warning) until
you delete the file. Use this when something is wrong with the rules
themselves, not as a way of life.

## Cross-platform

CI builds and tests on Linux, macOS, and Windows on every push. The
case-insensitive folder match in `forbidden_imports` is a deliberate
design choice — without it, the same `AetherLink.toml` would enforce
different rules on different developers' filesystems, which defeats
the entire point.

## License

MIT.
