pub mod config;
pub mod edit;
pub mod validator;

pub use config::{ForbiddenImport, Rules, RulesFile};
pub use edit::{
    add_rule_to_toml, remove_rule_from_toml, RemovalOutcome, RuleAddition, RuleRemoval,
};
pub use validator::validate;

/// A single broken rule, surfaced to the user with enough context to fix it.
#[derive(Debug, Clone)]
pub struct RuleViolation {
    pub rule: String,
    pub message: String,
    pub severity: Severity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}
