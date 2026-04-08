//! `aetherlink --install-hook` — cross-platform Claude Code hook installer.
//!
//! Patches `~/.claude/settings.json` to register a `PreToolUse` hook for
//! `Edit | Write | MultiEdit | NotebookEdit`. Idempotent: re-running just
//! refreshes the existing entry instead of duplicating it. Works on macOS,
//! Linux, and Windows because we never shell out — all JSON manipulation
//! is done in-process via `serde_json`.
//!
//! This is the portable replacement for the PowerShell block in install.bat.
//! `install.bat` keeps existing on Windows for the double-click experience,
//! but Mac/Linux users can now run `aetherlink --install-hook` directly and
//! the cargo-dist installers can call it as a post-install step.
//!
//! Layout patched into settings.json:
//!
//! ```json
//! {
//!   "hooks": {
//!     "PreToolUse": [
//!       {
//!         "matcher": "Edit|Write|MultiEdit|NotebookEdit",
//!         "hooks": [
//!           { "type": "command", "command": "<aetherlink-path> --hook-check" }
//!         ]
//!       }
//!     ]
//!   }
//! }
//! ```
//!
//! Use `--uninstall-hook` to remove the entry.

use std::env;
use std::fs;
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};

/// Marker we put on every entry we own so we can find and replace it on
/// re-install and remove it on uninstall without touching the user's
/// other hooks. Stored as a sibling field — Claude Code ignores
/// unknown keys in hook entries.
const OWNER_TAG: &str = "aetherlink";

pub fn install() -> Result<()> {
    let exe = env::current_exe().context("locating current executable")?;
    let exe_str = exe.to_string_lossy().into_owned();

    let settings_path = claude_code_settings_path()?;
    eprintln!("Patching {}", settings_path.display());

    let mut root = load_or_default(&settings_path)?;
    upsert_hook_entry(&mut root, &exe_str)?;
    write_atomically(&settings_path, &root)?;

    eprintln!("Installed PreToolUse hook for Edit|Write|MultiEdit|NotebookEdit.");
    eprintln!("Hook command: {} --hook-check", exe_str);
    eprintln!("Restart Claude Code for the change to take effect.");
    Ok(())
}

pub fn uninstall() -> Result<()> {
    let settings_path = claude_code_settings_path()?;
    if !settings_path.exists() {
        eprintln!("No Claude Code settings.json at {}; nothing to do.", settings_path.display());
        return Ok(());
    }

    let mut root = load_or_default(&settings_path)?;
    let removed = remove_hook_entry(&mut root);
    write_atomically(&settings_path, &root)?;

    if removed {
        eprintln!("Removed AetherLink PreToolUse hook from {}.", settings_path.display());
    } else {
        eprintln!("AetherLink hook not found in {} (already uninstalled).", settings_path.display());
    }
    Ok(())
}

/// Cross-platform location of Claude Code's user-level settings.json.
/// Documented as `~/.claude/settings.json` on every OS as of Claude Code
/// v0.x — the harness uses the same path on Mac/Linux/Windows. We still
/// use `home_dir`-style env lookups so the path works under MSYS, WSL,
/// and CI runners that don't set `HOME` to the canonical location.
pub fn claude_code_settings_path() -> Result<PathBuf> {
    let home = home_dir().ok_or_else(|| {
        anyhow!("could not determine home directory (HOME / USERPROFILE not set)")
    })?;
    let dir = home.join(".claude");
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir.join("settings.json"))
}

fn home_dir() -> Option<PathBuf> {
    if let Ok(h) = env::var("HOME") {
        if !h.is_empty() {
            return Some(PathBuf::from(h));
        }
    }
    if let Ok(u) = env::var("USERPROFILE") {
        if !u.is_empty() {
            return Some(PathBuf::from(u));
        }
    }
    None
}

fn load_or_default(path: &PathBuf) -> Result<Value> {
    if !path.exists() {
        return Ok(json!({}));
    }
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    // Strip a UTF-8 BOM if present (PowerShell and Notepad write one).
    let text = text.strip_prefix('\u{FEFF}').unwrap_or(&text);
    if text.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(text).with_context(|| format!("parsing {}", path.display()))
}

/// Insert (or replace) the AetherLink PreToolUse entry. Existing entries
/// for other tools are preserved. The owner tag lets us find our own
/// previous entry across re-installs.
fn upsert_hook_entry(root: &mut Value, exe: &str) -> Result<()> {
    let obj = root
        .as_object_mut()
        .ok_or_else(|| anyhow!("settings.json root is not a JSON object"))?;

    let hooks = obj
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| anyhow!("'hooks' exists but is not a JSON object"))?;

    let pre_tool_use = hooks
        .entry("PreToolUse")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .ok_or_else(|| anyhow!("'hooks.PreToolUse' exists but is not a JSON array"))?;

    // Drop any pre-existing aetherlink-owned entry, then push the fresh one.
    pre_tool_use.retain(|e| !is_owned_entry(e));

    let entry = json!({
        "matcher": "Edit|Write|MultiEdit|NotebookEdit",
        "owner": OWNER_TAG,
        "hooks": [
            {
                "type": "command",
                "command": format!("{exe} --hook-check")
            }
        ]
    });
    pre_tool_use.push(entry);
    Ok(())
}

fn remove_hook_entry(root: &mut Value) -> bool {
    let Some(obj) = root.as_object_mut() else { return false };
    let Some(hooks) = obj.get_mut("hooks").and_then(|h| h.as_object_mut()) else {
        return false;
    };
    let Some(pre) = hooks.get_mut("PreToolUse").and_then(|p| p.as_array_mut()) else {
        return false;
    };
    let before = pre.len();
    pre.retain(|e| !is_owned_entry(e));
    pre.len() != before
}

fn is_owned_entry(v: &Value) -> bool {
    v.get("owner").and_then(|o| o.as_str()) == Some(OWNER_TAG)
}

/// Write LF-terminated pretty JSON. We deliberately do *not* use
/// `\r\n` even on Windows: serde_json defaults to LF and Claude Code
/// reads either, but git's autocrlf can otherwise create spurious
/// diffs in user dotfiles.
fn write_atomically(path: &PathBuf, value: &Value) -> Result<()> {
    let mut text = serde_json::to_string_pretty(value)?;
    text.push('\n');
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, &text).with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("renaming into {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir() -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = env::temp_dir().join(format!("aetherlink-hook-{nanos}"));
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn upsert_into_empty_settings() {
        let mut root = json!({});
        upsert_hook_entry(&mut root, "/usr/bin/aetherlink").unwrap();
        let entry = &root["hooks"]["PreToolUse"][0];
        assert_eq!(entry["matcher"], "Edit|Write|MultiEdit|NotebookEdit");
        assert_eq!(entry["owner"], OWNER_TAG);
        assert_eq!(entry["hooks"][0]["command"], "/usr/bin/aetherlink --hook-check");
    }

    #[test]
    fn upsert_preserves_unrelated_hooks() {
        let mut root = json!({
            "hooks": {
                "PreToolUse": [
                    { "matcher": "Bash", "hooks": [{ "type": "command", "command": "echo bash" }] }
                ],
                "PostToolUse": [
                    { "matcher": "Edit", "hooks": [{ "type": "command", "command": "echo done" }] }
                ]
            },
            "otherKey": 42
        });
        upsert_hook_entry(&mut root, "/bin/al").unwrap();
        let pre = root["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 2, "the Bash hook must survive");
        assert_eq!(root["hooks"]["PostToolUse"][0]["matcher"], "Edit");
        assert_eq!(root["otherKey"], 42);
    }

    #[test]
    fn upsert_is_idempotent() {
        let mut root = json!({});
        upsert_hook_entry(&mut root, "/old/path").unwrap();
        upsert_hook_entry(&mut root, "/new/path").unwrap();
        let pre = root["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 1, "second install must replace, not duplicate");
        assert_eq!(pre[0]["hooks"][0]["command"], "/new/path --hook-check");
    }

    #[test]
    fn uninstall_removes_only_owned_entry() {
        let mut root = json!({
            "hooks": {
                "PreToolUse": [
                    { "matcher": "Bash", "hooks": [{ "type": "command", "command": "echo bash" }] }
                ]
            }
        });
        upsert_hook_entry(&mut root, "/bin/al").unwrap();
        assert_eq!(root["hooks"]["PreToolUse"].as_array().unwrap().len(), 2);
        let removed = remove_hook_entry(&mut root);
        assert!(removed);
        let pre = root["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 1);
        assert_eq!(pre[0]["matcher"], "Bash");
    }

    #[test]
    fn uninstall_on_clean_settings_is_noop() {
        let mut root = json!({ "hooks": { "PreToolUse": [] } });
        let removed = remove_hook_entry(&mut root);
        assert!(!removed);
    }

    #[test]
    fn write_uses_lf_line_endings() {
        let dir = tempdir();
        let path = dir.join("settings.json");
        let v = json!({ "hooks": { "PreToolUse": [] } });
        write_atomically(&path, &v).unwrap();
        let bytes = fs::read(&path).unwrap();
        assert!(!bytes.contains(&b'\r'), "settings.json must be written with LF line endings");
    }

    #[test]
    fn tolerates_utf8_bom() {
        let dir = tempdir();
        let path = dir.join("settings.json");
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(br#"{"hooks":{}}"#);
        fs::write(&path, &bytes).unwrap();
        let root = load_or_default(&path).unwrap();
        assert!(root.get("hooks").is_some());
    }
}
