# AetherLink

AetherLink is an architectural guardrail for AI coding agents, written in Rust. It runs as an MCP server and as a Claude Code `PreToolUse` hook, and it rejects a file write before it reaches disk if the write would break a rule the project has declared: a file growing past a line limit, a forbidden cross-folder import, or a circular dependency.

It is for developers who let agents such as Claude Code, Claude Desktop, or Cursor edit a codebase and want its structure to stay intact without reviewing every edit by hand.

## The problem

Agents optimise for the task in front of them. Left alone they grow files without limit, import the database layer straight from UI code, and introduce import cycles. Linters catch this after the fact, in CI, when the change is already written.

An MCP tool alone does not solve it either, because the agent only calls an MCP tool if it chooses to. AetherLink therefore also installs as a hook that the Claude Code harness runs before every `Edit`, `Write`, and `MultiEdit` call. The agent does not need to know AetherLink exists. A blocked write exits with code 2 and the reason goes back to the model on stderr, so it can fix the change and retry.

## What it does

- **Three rules**, declared in `AetherLink.toml`: `max_file_lines`, `forbidden_imports` (`"ui -> db"`), and `no_cycles`.
- **Validates the proposed state, not the current one.** The new file content is parsed in memory, substituted into a fresh scan of the project, and the whole import graph is re-checked before anything is written.
- **Write-time hook** (`aetherlink --hook-check`): reconstructs the post-edit content from the `Edit` / `Write` / `MultiEdit` payload and allows or blocks it.
- **Seven MCP tools**: `scan_project`, `check_change` (dry run), `apply_guarded_change` (validate, then write in the same call), `add_rule`, `remove_rule`, `acquire_lease`, `release_lease`.
- **Baseline ratchet** for existing codebases: `aetherlink --baseline` snapshots current violations into `.aetherlink-baseline.json`. Those are reported as warnings and stop blocking; any new violation still blocks.
- **File leases**: a 5-minute exclusive editing lease per file, so concurrent agents sharing one server can coordinate.
- **Path sandbox**: `check_change` and `apply_guarded_change` canonicalise the target (symlinks, `..`, files that do not exist yet) and refuse anything outside the project root.
- **Desktop feedback**: OS toast notifications on blocked writes and failed scans, plus an optional tray icon (green, red, or grey) showing the latest project state.
- **Languages**: Rust, Python, JavaScript, and TypeScript (`.rs`, `.py`, `.js`, `.jsx`, `.ts`, `.tsx`), parsed with tree-sitter.

## How it works

```
 Claude Code hook (stdin JSON)        MCP client (JSON-RPC over stdio)
            |                                      |
        hook.rs                              mcp/server.rs
            \______________________  ______________/
                                   \/
        scanner  ->  graph  ->  rules/validator  ->  baseline ratchet
       (walkdir,    (petgraph,   (line limit,        (committed baseline
      tree-sitter)  Tarjan SCC)  forbidden edges,     + on-disk state)
                                 cycles)
                                   |
                            allow / block
              (MCP path also: status.json -> tray icon, toasts)
```

| Module | Responsibility |
| --- | --- |
| `src/scanner/` | Walks the project (skipping `target`, `node_modules`, `.git`, `dist`, `build`, virtualenvs), runs tree-sitter queries per language to extract imports, and counts lines. |
| `src/graph/` | Resolves import strings to files and builds a `petgraph` directed graph. Handles Rust `crate::` / `super::` / `mod` paths, Python relative and package imports, JS/TS relative imports with `index.*` fallback, and `tsconfig.json` / `jsconfig.json` path aliases such as `@/*`. Cycles are found with Tarjan's strongly connected components. |
| `src/rules/` | `config.rs` parses `AetherLink.toml`; `validator.rs` runs the three checks; `baseline.rs` implements the ratchet; `edit.rs` adds and removes rules in the TOML text for `add_rule`, `remove_rule`, and `--add`. |
| `src/mcp/server.rs` | A hand-written JSON-RPC 2.0 loop on stdio using `tokio` (protocol version `2024-11-05`), with no MCP SDK. Implements `initialize`, `tools/list`, and `tools/call`. |
| `src/mcp/lease.rs` | In-memory lease registry with injectable time for tests. |
| `src/hook.rs` | The `PreToolUse` entry point. Finds the project by walking up from the target file to the nearest `AetherLink.toml`. |
| `src/security.rs` | Path sandbox and the `.aetherlink_bypass` switch. |
| `src/install_hook.rs`, `src/register.rs` | Idempotent installers that merge into `~/.claude/settings.json` and Claude Desktop's `claude_desktop_config.json` without disturbing other entries, and tolerate a UTF-8 BOM. |
| `src/status.rs`, `src/tray.rs`, `src/notify.rs` | The server writes `status.json`; the tray process polls it every 2 seconds; toasts are best-effort and never fail the server. |

**What blocks a write.** A violation blocks only if it is error severity, is not in the committed baseline, and is either new relative to the on-disk project or mentions the file being written. Unrelated existing violations never block an edit, but touching a file that is itself in violation does. The hook and the MCP tools apply the same logic.

**Design decisions visible in the code**

- The hook fails open. A malformed payload, a file outside any AetherLink project, an unsupported extension, or an internal error all allow the write, so a bug in AetherLink cannot stall the agent loop.
- `forbidden_imports` matches folder names case-insensitively, so one `AetherLink.toml` enforces the same boundary on Linux, macOS, and Windows.
- The sandbox check runs before the bypass check: `.aetherlink_bypass` disables rule enforcement but never permits a write outside the project root.

## Tech stack

Rust 2021. `tokio` (async stdio), `serde` / `serde_json` / `toml`, `tree-sitter` with the Rust, Python, JavaScript, and TypeScript grammars, `petgraph`, `walkdir`, `glob`, `dialoguer` (interactive CLI), `tray-icon` + `tao` (system tray), `notify-rust` (toasts), `anyhow`, `tracing`. The release profile uses LTO, a single codegen unit, `panic = "abort"`, and symbol stripping.

The crate has 69 unit tests covering the validator, config parsing, TOML editing, baseline, leases, the path sandbox, the installers, and the import parser.

## Install

There is no published release yet, so build from source with a stable Rust toolchain:

```sh
git clone https://github.com/DBrokenAI/aetherlink
cd aetherlink
cargo build --release
./target/release/aetherlink --register        # Claude Desktop + starter AetherLink.toml
./target/release/aetherlink --install-hook    # Claude Code PreToolUse hook
```

On Linux the tray dependencies need system packages first: `libgtk-3-dev libxdo-dev libayatana-appindicator3-dev libssl-dev pkg-config`.

On Windows, `install.bat` does the same after a release build: it copies the binary to `%LOCALAPPDATA%\AetherLink`, registers it with Claude Desktop, with the Claude Code CLI (`claude mcp add`) if present, and with Cursor (`~/.cursor/mcp.json`) if present, and installs the hook.

Restart the client afterwards; MCP servers and hooks are loaded at startup.

## Usage

```
aetherlink                   Run as an MCP server on stdio (default)
aetherlink --register        Add to Claude Desktop's config; create a starter
                             AetherLink.toml in the current folder if missing
aetherlink --install-hook    Add the PreToolUse hook to ~/.claude/settings.json
aetherlink --uninstall-hook  Remove only the AetherLink hook entry
aetherlink --hook-check      Hook entry point: reads the tool payload on stdin,
                             exits 0 (allow) or 2 (block)
aetherlink --add             Interactive menu to add or remove a rule, then re-scan
aetherlink --baseline [PATH] Snapshot current violations into .aetherlink-baseline.json
aetherlink --tray            Run the system tray indicator
aetherlink --version | --help
```

### Configuration

`AetherLink.toml` in the project root (this is the file shipped in this repo and written by `--register`):

```toml
[rules]
# Reject any source file longer than this many lines.
max_file_lines = 500

# Block circular dependencies in the import graph.
no_cycles = true

# Block specific cross-folder imports. Format: "from -> to".
# Each side matches any directory component of a file's path.
# Example: forbidden_imports = ["ui -> db", "api -> secret"]
forbidden_imports = []
```

Two further options are supported:

```toml
[rules]
default_severity = "warn"   # "error" (default) blocks; "warn" reports only

[[overrides]]               # last matching glob wins
path = "data/**"
max_file_lines = 5000
```

A project with no `AetherLink.toml` is not checked.

### Adopting it on an existing codebase

Run `aetherlink --baseline` in the project root and commit `.aetherlink-baseline.json`. Existing violations are then reported as warnings, and only new ones block. Re-run it after fixing violations to shrink the baseline.

### Bypass

Create an empty `.aetherlink_bypass` file in the project root. Writes are allowed through with a warning (a critical toast on the MCP path, a stderr note on the hook path) until the file is deleted. It is intended for cases where the rules themselves are wrong.

## CI and release setup

- `.github/workflows/ci.yml` runs on pushes and pull requests to `master` / `main` across Ubuntu, macOS, and Windows: `cargo fmt --check` and `cargo clippy -D warnings` (both advisory), `cargo build --release`, and `cargo test --all`.
- `.github/workflows/release.yml` is triggered by `v*.*.*` tags and uses cargo-dist to build five targets (x86_64 and aarch64 Linux, x86_64 and aarch64 macOS, x86_64 Windows) and attach them to a GitHub Release. The cargo-dist configuration lives in `Cargo.toml`.

## Status and limitations

- Early stage: `Cargo.toml` is at version 0.1.0 and no release has been published.
- The hook covers Claude Code's file-editing tools. Writes made another way, such as a shell command, are not intercepted.
- Every check re-scans the whole project; there is no incremental cache.
- Import resolution is heuristic. Imports that do not resolve to a project file are treated as external and ignored.
- `forbidden_imports` matches single folder names, not full paths. `[[overrides]]` applies only to `max_file_lines`, and severity is one global setting rather than per rule.
- Leases live in the memory of one server process and are advisory; `apply_guarded_change` does not require one.
- Files with other extensions are not validated by the hook and are refused by `apply_guarded_change`.

## License

`Cargo.toml` declares `license = "MIT"`. The repository does not yet contain a LICENSE file.
