//! Baseline / ratchet support.
//!
//! A baseline is a frozen snapshot of the violations that existed in the
//! project at the time `aetherlink --baseline` was run. Future scans and
//! guarded writes filter their violation set against the baseline:
//!
//!  * Violations with a fingerprint **in** the baseline are pre-existing rot.
//!    They are surfaced as warnings (so the user can still see the debt) but
//!    they do **not** block writes — even writes to the offending file. This
//!    is the "freeze the rot" half of the ratchet.
//!
//!  * Violations with a fingerprint **not** in the baseline are *new* rot
//!    introduced by the change being evaluated. These block as normal. This
//!    is the "prevent regression" half.
//!
//! The killer property is that no manual `.aetherlink_bypass` is needed to
//! install AetherLink on an existing codebase: you run `--baseline` once,
//! commit the baseline file, and from that moment on the project is *not*
//! allowed to get architecturally worse without a human deciding it can.
//!
//! As violations are fixed for real, they should be removed from the baseline
//! (run `--baseline` again to re-snapshot). Tightening rules and re-baselining
//! is the loop you walk to pay down debt.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::RuleViolation;

/// One entry in the baseline file. We store the full message (not just a
/// hash) so a human inspecting `.aetherlink-baseline.json` can see what was
/// grandfathered in. The fingerprint used for filtering is `rule::message`,
/// matching `RuleViolation::fingerprint`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaselineEntry {
    pub rule: String,
    pub message: String,
}

impl BaselineEntry {
    pub fn fingerprint(&self) -> String {
        format!("{}::{}", self.rule, self.message)
    }
}

/// The full baseline. The `version` field exists so future format changes can
/// be detected and migrated cleanly instead of silently misinterpreted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Baseline {
    pub version: u32,
    pub entries: Vec<BaselineEntry>,
}

impl Baseline {
    pub const FILE_NAME: &'static str = ".aetherlink-baseline.json";

    pub fn from_violations(violations: &[RuleViolation]) -> Self {
        Self {
            version: 1,
            entries: violations
                .iter()
                .map(|v| BaselineEntry {
                    rule: v.rule.clone(),
                    message: v.message.clone(),
                })
                .collect(),
        }
    }

    pub fn path_in(project_root: &Path) -> PathBuf {
        project_root.join(Self::FILE_NAME)
    }

    /// Load the baseline file if it exists. Missing file = `None`, not an
    /// error: the baseline is opt-in, and projects without one fall back to
    /// the original "every violation blocks" behavior.
    pub fn load(project_root: &Path) -> Result<Option<Self>> {
        let path = Self::path_in(project_root);
        if !path.exists() {
            return Ok(None);
        }
        let text = fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let parsed: Baseline = serde_json::from_str(&text)
            .with_context(|| format!("parsing {}", path.display()))?;
        Ok(Some(parsed))
    }

    pub fn save(&self, project_root: &Path) -> Result<PathBuf> {
        let path = Self::path_in(project_root);
        let text = serde_json::to_string_pretty(self)?;
        fs::write(&path, text)
            .with_context(|| format!("writing {}", path.display()))?;
        Ok(path)
    }

    pub fn fingerprints(&self) -> HashSet<String> {
        self.entries.iter().map(|e| e.fingerprint()).collect()
    }
}

/// Split a violation set into `(new, grandfathered)` using the baseline.
/// `new` are the violations that were *not* present at baseline time and
/// should block writes; `grandfathered` are pre-existing rot that should be
/// surfaced (e.g. in the report) but not block.
pub fn split_against_baseline(
    violations: Vec<RuleViolation>,
    baseline: Option<&Baseline>,
) -> (Vec<RuleViolation>, Vec<RuleViolation>) {
    let Some(baseline) = baseline else {
        return (violations, Vec::new());
    };
    let frozen = baseline.fingerprints();
    let mut new = Vec::new();
    let mut grandfathered = Vec::new();
    for v in violations {
        if frozen.contains(&v.fingerprint()) {
            grandfathered.push(v);
        } else {
            new.push(v);
        }
    }
    (new, grandfathered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::Severity;

    fn v(rule: &str, msg: &str) -> RuleViolation {
        RuleViolation {
            rule: rule.into(),
            message: msg.into(),
            severity: Severity::Error,
        }
    }

    #[test]
    fn split_with_no_baseline_returns_all_as_new() {
        let vs = vec![v("max_file_lines", "a 600 over 400")];
        let (new, old) = split_against_baseline(vs.clone(), None);
        assert_eq!(new.len(), 1);
        assert!(old.is_empty());
    }

    #[test]
    fn split_grandfathers_known_violations() {
        let baseline = Baseline::from_violations(&[v("max_file_lines", "huge.js 9000 over 400")]);
        let vs = vec![
            v("max_file_lines", "huge.js 9000 over 400"), // grandfathered
            v("max_file_lines", "new.js 500 over 400"),    // new
        ];
        let (new, old) = split_against_baseline(vs, Some(&baseline));
        assert_eq!(new.len(), 1);
        assert_eq!(new[0].message, "new.js 500 over 400");
        assert_eq!(old.len(), 1);
    }
}
