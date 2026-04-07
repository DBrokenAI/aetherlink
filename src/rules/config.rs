use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

/// Top-level shape of `AetherLink.toml`.
///
/// Example:
/// ```toml
/// [rules]
/// max_file_lines = 500
/// no_cycles = true
/// forbidden_imports = ["ui -> db", "api -> secret"]
/// ```
#[derive(Debug, Clone, Deserialize)]
pub struct RulesFile {
    #[serde(default)]
    pub rules: Rules,
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
        let path = Self::config_path(project_root);
        if !path.exists() {
            tracing::info!("No AetherLink.toml found at {}; using empty ruleset", path.display());
            return Ok(Self::default());
        }
        Self::load_from_file(&path)
    }

    pub fn load_from_file(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        Self::from_toml_str(&text)
            .with_context(|| format!("parsing {}", path.display()))
    }

    pub fn from_toml_str(text: &str) -> Result<Self> {
        let parsed: RulesFile = toml::from_str(text)?;
        Ok(parsed.rules)
    }

    pub fn config_path(project_root: &Path) -> PathBuf {
        project_root.join("AetherLink.toml")
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
        assert_eq!(rules.forbidden_imports[0], ForbiddenImport { from: "ui".into(), to: "db".into() });
        assert_eq!(rules.forbidden_imports[1], ForbiddenImport { from: "api".into(), to: "secret".into() });
    }

    #[test]
    fn empty_config_yields_defaults() {
        let rules = Rules::from_toml_str("").unwrap();
        assert_eq!(rules.max_file_lines, None);
        assert!(!rules.no_cycles);
        assert!(rules.forbidden_imports.is_empty());
    }

    #[test]
    fn partial_config_works() {
        let rules = Rules::from_toml_str("[rules]\nno_cycles = true\n").unwrap();
        assert!(rules.no_cycles);
        assert_eq!(rules.max_file_lines, None);
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
}
