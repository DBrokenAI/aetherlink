use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

use super::Severity;

/// Top-level shape of `AetherLink.toml`.
///
/// Example:
/// ```toml
/// [rules]
/// max_file_lines = 500
/// no_cycles = true
/// forbidden_imports = ["ui -> db", "api -> secret"]
/// default_severity = "error"   # or "warn"
///
/// [[overrides]]
/// path = "data/**"
/// max_file_lines = 5000
/// ```
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RulesFile {
    #[serde(default)]
    pub rules: Rules,

    /// Per-folder/per-glob rule overrides. Each entry can relax (or tighten)
    /// `max_file_lines` for files matching its `path` glob. Order matters:
    /// the *last* matching override wins, so put more specific patterns last.
    #[serde(default)]
    pub overrides: Vec<RuleOverride>,
}

/// The architectural laws AetherLink enforces.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Rules {
    /// Maximum allowed line count for any single source file.
    /// `None` disables the check.
    pub max_file_lines: Option<usize>,

    /// Forbidden folder-to-folder import edges.
    /// Each entry is parsed from `"from -> to"` syntax.
    #[serde(default, deserialize_with = "deserialize_forbidden_imports")]
    pub forbidden_imports: Vec<ForbiddenImport>,

    /// If true, the dependency graph must be acyclic.
    #[serde(default)]
    pub no_cycles: bool,

    /// Default severity assigned to every violation. `error` blocks writes,
    /// `warn` surfaces the violation in reports but allows the write through.
    /// Per-rule severity is on the roadmap; for now this is global.
    #[serde(default = "default_severity_value")]
    pub default_severity: Severity,
}

fn default_severity_value() -> Severity {
    Severity::Error
}

/// A rule override scoped to a path glob. Currently only relaxes
/// `max_file_lines`, which is the most-requested override (e.g. allowing
/// large data dumps under `data/**` while keeping a tight limit on `src/`).
#[derive(Debug, Clone, Deserialize)]
pub struct RuleOverride {
    /// Glob pattern matched against a file's path *relative to project root*.
    /// Examples: `data/**`, `**/recipes.js`, `src/legacy/**`.
    pub path: String,
    /// If set, replaces `max_file_lines` for files matching this override.
    pub max_file_lines: Option<usize>,
}

/// A single forbidden-import rule: code under `from` must not import from code under `to`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForbiddenImport {
    pub from: String,
    pub to: String,
}

impl ForbiddenImport {
    pub fn parse(raw: &str) -> Result<Self> {
        let (from, to) = raw
            .split_once("->")
            .ok_or_else(|| anyhow!("forbidden_imports entry must be 'from -> to', got: {raw}"))?;
        let from = from.trim().to_string();
        let to = to.trim().to_string();
        if from.is_empty() || to.is_empty() {
            return Err(anyhow!("forbidden_imports entry has empty side: {raw}"));
        }
        Ok(Self { from, to })
    }
}

fn deserialize_forbidden_imports<'de, D>(de: D) -> Result<Vec<ForbiddenImport>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let raw: Vec<String> = Vec::deserialize(de)?;
    raw.into_iter()
        .map(|s| ForbiddenImport::parse(&s).map_err(D::Error::custom))
        .collect()
}

impl Rules {
    /// Load `AetherLink.toml` from the given project root.
    /// Returns default (empty) rules if the file does not exist.
    pub fn load(project_root: &Path) -> Result<Self> {
        Ok(RulesFile::load(project_root)?.rules)
    }

    pub fn from_toml_str(text: &str) -> Result<Self> {
        Ok(RulesFile::from_toml_str(text)?.rules)
    }

    pub fn config_path(project_root: &Path) -> PathBuf {
        project_root.join("AetherLink.toml")
    }
}

impl RulesFile {
    pub fn load(project_root: &Path) -> Result<Self> {
        let path = Rules::config_path(project_root);
        if !path.exists() {
            tracing::info!(
                "No AetherLink.toml found at {}; using empty ruleset",
                path.display()
            );
            return Ok(Self::default());
        }
        let text = fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        Self::from_toml_str(&text)
            .with_context(|| format!("parsing {}", path.display()))
    }

    pub fn from_toml_str(text: &str) -> Result<Self> {
        let parsed: RulesFile = toml::from_str(text)?;
        Ok(parsed)
    }

    /// Resolve the effective `max_file_lines` for a specific file. Walks the
    /// `overrides` list in order; the last matching override wins (so users can
    /// stack a broad rule + a more specific exception).
    pub fn effective_max_file_lines(&self, file_rel_path: &Path) -> Option<usize> {
        let mut effective = self.rules.max_file_lines;
        let path_str = file_rel_path.to_string_lossy().replace('\\', "/");
        for ov in &self.overrides {
            if let Ok(pat) = glob::Pattern::new(&ov.path) {
                if pat.matches(&path_str) && ov.max_file_lines.is_some() {
                    effective = ov.max_file_lines;
                }
            }
        }
        effective
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_config() {
        let toml = r#"
            [rules]
            max_file_lines = 500
            no_cycles = true
            forbidden_imports = ["ui -> db", "api -> secret"]
        "#;
        let rules = Rules::from_toml_str(toml).unwrap();
        assert_eq!(rules.max_file_lines, Some(500));
        assert!(rules.no_cycles);
        assert_eq!(rules.forbidden_imports.len(), 2);
        assert_eq!(
            rules.forbidden_imports[0],
            ForbiddenImport {
                from: "ui".into(),
                to: "db".into()
            }
        );
        assert_eq!(rules.default_severity, Severity::Error);
    }

    #[test]
    fn empty_config_yields_defaults() {
        let rules = Rules::from_toml_str("").unwrap();
        assert_eq!(rules.max_file_lines, None);
        assert!(!rules.no_cycles);
        assert!(rules.forbidden_imports.is_empty());
    }

    #[test]
    fn rejects_malformed_forbidden_import() {
        let toml = r#"
            [rules]
            forbidden_imports = ["ui to db"]
        "#;
        assert!(Rules::from_toml_str(toml).is_err());
    }

    #[test]
    fn forbidden_import_parse_trims_whitespace() {
        let f = ForbiddenImport::parse("  ui   ->   db  ").unwrap();
        assert_eq!(f.from, "ui");
        assert_eq!(f.to, "db");
    }

    #[test]
    fn override_relaxes_max_lines_for_matching_path() {
        let toml = r#"
            [rules]
            max_file_lines = 400

            [[overrides]]
            path = "data/**"
            max_file_lines = 9999
        "#;
        let file = RulesFile::from_toml_str(toml).unwrap();
        assert_eq!(
            file.effective_max_file_lines(Path::new("src/main.js")),
            Some(400)
        );
        assert_eq!(
            file.effective_max_file_lines(Path::new("data/recipes.js")),
            Some(9999)
        );
        assert_eq!(
            file.effective_max_file_lines(Path::new("data/sub/big.js")),
            Some(9999)
        );
    }

    #[test]
    fn parses_severity() {
        let toml = r#"
            [rules]
            max_file_lines = 100
            default_severity = "warn"
        "#;
        let rules = Rules::from_toml_str(toml).unwrap();
        assert_eq!(rules.default_severity, Severity::Warning);
    }
}
