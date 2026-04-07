pub mod baseline;
pub mod config;
pub mod edit;
pub mod validator;

use serde::{Deserialize, Serialize};

pub use baseline::{Baseline, BaselineEntry};
pub use config::{ForbiddenImport, RuleOverride, Rules, RulesFile};
pub use edit::{
    add_rule_to_toml, remove_rule_from_toml, RemovalOutcome, RuleAddition, RuleRemoval,
};
pub use validator::{validate, validate_with_overrides};

/// A single broken rule, surfaced to the user with enough context to fix it.
#[derive(Debug, Clone)]
pub struct RuleViolation {
    pub rule: String,
    pub message: String,
    pub severity: Severity,
}

impl RuleViolation {
    /// Stable identifier used for baseline matching. Two violations with the
    /// same rule and message are considered "the same problem" — that's how
    /// the ratchet decides whether a violation is pre-existing or new.
    pub fn fingerprint(&self) -> String {
        format!("{}::{}", self.rule, self.message)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    #[serde(alias = "warn")]
    Warning,
}

impl Default for Severity {
    fn default() -> Self {
        Severity::Error
    }
}
