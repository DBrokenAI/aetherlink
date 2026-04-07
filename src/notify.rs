//! OS-native toast notifications for AetherLink events.
//!
//! Best-effort: notification failures (no display server, no notification
//! daemon, missing AppUserModelID on Windows, etc.) are silently ignored. The
//! MCP server must never fail because a toast couldn't be shown.

use notify_rust::{Notification, Timeout};

const APP_NAME: &str = "AetherLink";

/// Fire a toast for a `WRITE BLOCKED` from `check_change`.
pub fn write_blocked(file_path: &str, violation_count: usize) {
    let body = if violation_count == 1 {
        format!("1 architectural violation in {file_path}")
    } else {
        format!("{violation_count} architectural violations in {file_path}")
    };
    show("AetherLink: Write Blocked", &body);
}

/// Fire a toast when `scan_project` finds the project illegal.
pub fn scan_illegal(project_path: &str, violation_count: usize) {
    let body = if violation_count == 1 {
        format!("1 violation in {project_path}")
    } else {
        format!("{violation_count} violations in {project_path}")
    };
    show("AetherLink: Project ILLEGAL", &body);
}

/// Fire a CRITICAL warning when `.aetherlink_bypass` is engaged and a write
/// went through without validation. The user wanted to know whenever the
/// safety net is off, so this fires on every bypassed write.
pub fn bypass_active(file_path: &str, violation_count: usize) {
    let body = if violation_count == 0 {
        format!("Wrote {file_path} without validation (project was clean anyway)")
    } else {
        format!(
            "Wrote {file_path} despite {violation_count} architectural violation{}. Validation was BYPASSED.",
            if violation_count == 1 { "" } else { "s" }
        )
    };
    show("AetherLink: BYPASS ACTIVE — CRITICAL", &body);
}

/// Fire a toast when a path-escape attempt is blocked. This shouldn't happen
/// in normal usage; if you see one, an agent tried to write outside the
/// project root and AetherLink stopped it.
pub fn path_escape_blocked(attempted: &str) {
    show(
        "AetherLink: PATH ESCAPE BLOCKED — CRITICAL",
        &format!("Refused to write outside the project root: {attempted}"),
    );
}

/// Fire a toast when a previously-illegal project becomes legal again.
pub fn scan_legal(project_path: &str) {
    show(
        "AetherLink: Project Legal",
        &format!("All architectural rules pass in {project_path}"),
    );
}

fn show(summary: &str, body: &str) {
    let result = Notification::new()
        .appname(APP_NAME)
        .summary(summary)
        .body(body)
        .timeout(Timeout::Milliseconds(5000))
        .show();
    if let Err(e) = result {
        tracing::warn!("toast notification failed: {e}");
    }
}
