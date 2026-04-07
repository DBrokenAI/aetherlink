//! Cross-process status file.
//!
//! The MCP server writes the latest scan/check result here. The `--tray`
//! process polls it and updates the system tray icon accordingly. Plain JSON
//! on disk so any other process (or a curious user with `cat`) can read it.

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Legal,
    Illegal,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViolationSummary {
    pub rule: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Status {
    pub state: State,
    pub project_path: String,
    pub violation_count: usize,
    pub violations: Vec<ViolationSummary>,
    /// Source of the update — "scan_project" or "check_change" — so the tray
    /// tooltip can show why the state changed.
    pub source: String,
    pub updated_at_secs: u64,
}

impl Status {
    pub fn now(
        state: State,
        project_path: impl Into<String>,
        violations: Vec<ViolationSummary>,
        source: impl Into<String>,
    ) -> Self {
        Self {
            state,
            project_path: project_path.into(),
            violation_count: violations.len(),
            violations,
            source: source.into(),
            updated_at_secs: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        }
    }
}

/// Per-platform location of the status file. Parent directory is created on
/// demand so callers don't need to.
pub fn status_file_path() -> Result<PathBuf> {
    let dir = if cfg!(target_os = "windows") {
        let appdata = std::env::var("APPDATA")
            .map_err(|_| anyhow!("APPDATA environment variable is not set"))?;
        PathBuf::from(appdata).join("AetherLink")
    } else if cfg!(target_os = "macos") {
        let home = std::env::var("HOME").map_err(|_| anyhow!("HOME is not set"))?;
        PathBuf::from(home)
            .join("Library")
            .join("Application Support")
            .join("AetherLink")
    } else {
        let home = std::env::var("HOME").map_err(|_| anyhow!("HOME is not set"))?;
        PathBuf::from(home).join(".config").join("AetherLink")
    };
    fs::create_dir_all(&dir)
        .with_context(|| format!("creating status directory {}", dir.display()))?;
    Ok(dir.join("status.json"))
}

/// Best-effort write — the MCP server should never fail because notifications
/// couldn't reach the tray. Errors are logged at warn level and swallowed.
pub fn write(status: &Status) {
    if let Err(e) = try_write(status) {
        tracing::warn!("status write failed: {e}");
    }
}

fn try_write(status: &Status) -> Result<()> {
    let path = status_file_path()?;
    let text = serde_json::to_string_pretty(status)?;
    fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Read the latest status. Returns `Ok(None)` if the file doesn't exist yet.
pub fn read() -> Result<Option<Status>> {
    let path = status_file_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path)
        .with_context(|| format!("reading {}", path.display()))?;
    let status: Status = serde_json::from_str(&text)
        .with_context(|| format!("parsing {}", path.display()))?;
    Ok(Some(status))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_json() {
        let s = Status::now(
            State::Illegal,
            "/proj".to_string(),
            vec![ViolationSummary {
                rule: "max_file_lines".into(),
                message: "foo.rs has 600 lines".into(),
            }],
            "scan_project",
        );
        let text = serde_json::to_string(&s).unwrap();
        let parsed: Status = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed.state, State::Illegal);
        assert_eq!(parsed.violation_count, 1);
        assert_eq!(parsed.violations[0].rule, "max_file_lines");
        assert_eq!(parsed.source, "scan_project");
    }

    #[test]
    fn state_serializes_lowercase() {
        let s = Status::now(State::Legal, "/p".to_string(), vec![], "test");
        let text = serde_json::to_string(&s).unwrap();
        assert!(text.contains("\"state\":\"legal\""));
    }
}
