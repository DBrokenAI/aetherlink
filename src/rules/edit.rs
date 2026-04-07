//! In-place editing of `AetherLink.toml`.
//!
//! Used by both the `add_rule` MCP tool and the `aetherlink --add` interactive
//! CLI. Operates on the TOML text directly so callers can read the file, mutate
//! it, and write it back without round-tripping through the typed `Rules`
//! struct (which would lose any keys we don't yet model).

use anyhow::{anyhow, Context, Result};
use toml::{Table, Value};

/// A single rule the user wants to add. Each variant maps onto exactly one
/// field of `AetherLink.toml`'s `[rules]` section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleAddition {
    MaxFileLines(i64),
    NoCycles(bool),
    ForbiddenImport { from: String, to: String },
}

impl RuleAddition {
    /// Short label for log messages and CLI feedback.
    pub fn human_label(&self) -> String {
        match self {
            RuleAddition::MaxFileLines(n) => format!("max_file_lines = {n}"),
            RuleAddition::NoCycles(b) => format!("no_cycles = {b}"),
            RuleAddition::ForbiddenImport { from, to } => {
                format!("forbidden_imports += \"{from} -> {to}\"")
            }
        }
    }
}

/// Apply a rule addition to a TOML document and return the new text.
///
/// - Empty / missing input is treated as a fresh document.
/// - Existing rules under `[rules]` are preserved.
/// - Adding `forbidden_import` is idempotent: duplicates are skipped.
/// - Setting `max_file_lines` or `no_cycles` overwrites the previous value
///   (these are scalar settings, not a list).
pub fn add_rule_to_toml(existing: &str, addition: &RuleAddition) -> Result<String> {
    let mut doc: Table = if existing.trim().is_empty() {
        Table::new()
    } else {
        existing
            .parse::<Table>()
            .context("parsing existing AetherLink.toml")?
    };

    // Ensure `[rules]` exists and is a table.
    let rules_entry = doc
        .entry("rules".to_string())
        .or_insert_with(|| Value::Table(Table::new()));
    let rules = rules_entry
        .as_table_mut()
        .ok_or_else(|| anyhow!("'rules' exists in AetherLink.toml but is not a table"))?;

    match addition {
        RuleAddition::MaxFileLines(n) => {
            if *n <= 0 {
                return Err(anyhow!("max_file_lines must be positive, got {n}"));
            }
            rules.insert("max_file_lines".to_string(), Value::Integer(*n));
        }
        RuleAddition::NoCycles(b) => {
            rules.insert("no_cycles".to_string(), Value::Boolean(*b));
        }
        RuleAddition::ForbiddenImport { from, to } => {
            if from.is_empty() || to.is_empty() {
                return Err(anyhow!("forbidden_import 'from' and 'to' must be non-empty"));
            }
            let new_entry = format!("{from} -> {to}");
            let arr_entry = rules
                .entry("forbidden_imports".to_string())
                .or_insert_with(|| Value::Array(Vec::new()));
            let arr = arr_entry
                .as_array_mut()
                .ok_or_else(|| anyhow!("'forbidden_imports' exists but is not an array"))?;
            // Idempotent: skip if the same rule already exists (whitespace-insensitive).
            let already = arr.iter().any(|v| {
                v.as_str()
                    .map(|s| normalize_arrow_rule(s) == normalize_arrow_rule(&new_entry))
                    .unwrap_or(false)
            });
            if !already {
                arr.push(Value::String(new_entry));
            }
        }
    }

    toml::to_string_pretty(&Value::Table(doc))
        .context("serializing updated AetherLink.toml")
}

/// A rule the user wants to remove. Mirrors `RuleAddition` but for deletion:
/// scalar rules don't need a value (we just drop the key), and forbidden
/// imports need the exact (from, to) pair so we know which entry to drop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleRemoval {
    MaxFileLines,
    NoCycles,
    ForbiddenImport { from: String, to: String },
}

impl RuleRemoval {
    pub fn human_label(&self) -> String {
        match self {
            RuleRemoval::MaxFileLines => "max_file_lines".to_string(),
            RuleRemoval::NoCycles => "no_cycles".to_string(),
            RuleRemoval::ForbiddenImport { from, to } => {
                format!("forbidden_imports -= \"{from} -> {to}\"")
            }
        }
    }
}

/// Whether a `remove_rule_from_toml` call actually changed anything.
/// `NotPresent` lets callers tell the user "that rule wasn't set" instead of
/// pretending we did work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemovalOutcome {
    Removed,
    NotPresent,
}

/// Apply a rule removal to a TOML document and return the new text plus an
/// outcome flag.
///
/// - Empty input is a no-op (`NotPresent`).
/// - Removing a `forbidden_import` only drops the matching entry; other
///   forbidden imports are preserved. If the array becomes empty, the key
///   itself is removed (so the file stays tidy).
/// - Removing a non-existent rule is **not an error**; it just returns
///   `NotPresent` so the caller can decide how to phrase it.
pub fn remove_rule_from_toml(
    existing: &str,
    removal: &RuleRemoval,
) -> Result<(String, RemovalOutcome)> {
    if existing.trim().is_empty() {
        return Ok((String::new(), RemovalOutcome::NotPresent));
    }

    let mut doc: Table = existing
        .parse::<Table>()
        .context("parsing existing AetherLink.toml")?;

    // No `[rules]` section means nothing to remove.
    let Some(rules_value) = doc.get_mut("rules") else {
        let out = toml::to_string_pretty(&Value::Table(doc))?;
        return Ok((out, RemovalOutcome::NotPresent));
    };
    let rules = rules_value
        .as_table_mut()
        .ok_or_else(|| anyhow!("'rules' exists in AetherLink.toml but is not a table"))?;

    let outcome = match removal {
        RuleRemoval::MaxFileLines => {
            if rules.remove("max_file_lines").is_some() {
                RemovalOutcome::Removed
            } else {
                RemovalOutcome::NotPresent
            }
        }
        RuleRemoval::NoCycles => {
            if rules.remove("no_cycles").is_some() {
                RemovalOutcome::Removed
            } else {
                RemovalOutcome::NotPresent
            }
        }
        RuleRemoval::ForbiddenImport { from, to } => {
            let target = format!("{from} -> {to}");
            let target_norm = normalize_arrow_rule(&target);
            let Some(arr_value) = rules.get_mut("forbidden_imports") else {
                return finish(doc, RemovalOutcome::NotPresent);
            };
            let arr = arr_value
                .as_array_mut()
                .ok_or_else(|| anyhow!("'forbidden_imports' exists but is not an array"))?;
            let before = arr.len();
            arr.retain(|v| {
                v.as_str()
                    .map(|s| normalize_arrow_rule(s) != target_norm)
                    .unwrap_or(true)
            });
            let removed = arr.len() < before;
            // Tidy up: drop the key if the array is now empty.
            if arr.is_empty() {
                rules.remove("forbidden_imports");
            }
            if removed {
                RemovalOutcome::Removed
            } else {
                RemovalOutcome::NotPresent
            }
        }
    };

    finish(doc, outcome)
}

fn finish(doc: Table, outcome: RemovalOutcome) -> Result<(String, RemovalOutcome)> {
    let out = toml::to_string_pretty(&Value::Table(doc))?;
    Ok((out, outcome))
}

fn normalize_arrow_rule(s: &str) -> String {
    s.split("->")
        .map(|p| p.trim())
        .collect::<Vec<_>>()
        .join(" -> ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules_table(text: &str) -> Table {
        let parsed: Table = text.parse().unwrap();
        parsed["rules"].as_table().unwrap().clone()
    }

    #[test]
    fn adds_max_file_lines_to_empty_file() {
        let out = add_rule_to_toml("", &RuleAddition::MaxFileLines(500)).unwrap();
        let r = rules_table(&out);
        assert_eq!(r["max_file_lines"].as_integer(), Some(500));
    }

    #[test]
    fn adds_no_cycles_to_empty_file() {
        let out = add_rule_to_toml("", &RuleAddition::NoCycles(true)).unwrap();
        let r = rules_table(&out);
        assert_eq!(r["no_cycles"].as_bool(), Some(true));
    }

    #[test]
    fn adds_forbidden_import_to_empty_file() {
        let out = add_rule_to_toml(
            "",
            &RuleAddition::ForbiddenImport {
                from: "ui".into(),
                to: "db".into(),
            },
        )
        .unwrap();
        let r = rules_table(&out);
        let arr = r["forbidden_imports"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0].as_str(), Some("ui -> db"));
    }

    #[test]
    fn preserves_existing_rules() {
        let existing = r#"
            [rules]
            max_file_lines = 200
            forbidden_imports = ["api -> secret"]
        "#;
        let out = add_rule_to_toml(
            existing,
            &RuleAddition::ForbiddenImport {
                from: "ui".into(),
                to: "db".into(),
            },
        )
        .unwrap();
        let r = rules_table(&out);
        // Old scalar still there
        assert_eq!(r["max_file_lines"].as_integer(), Some(200));
        // Both forbidden imports present, in order
        let arr = r["forbidden_imports"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0].as_str(), Some("api -> secret"));
        assert_eq!(arr[1].as_str(), Some("ui -> db"));
    }

    #[test]
    fn forbidden_import_is_idempotent() {
        let existing = r#"
            [rules]
            forbidden_imports = ["ui -> db"]
        "#;
        // Same rule, different whitespace.
        let out = add_rule_to_toml(
            existing,
            &RuleAddition::ForbiddenImport {
                from: "ui".into(),
                to: "db".into(),
            },
        )
        .unwrap();
        let r = rules_table(&out);
        assert_eq!(r["forbidden_imports"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn max_file_lines_overwrites_previous() {
        let existing = "[rules]\nmax_file_lines = 100\n";
        let out = add_rule_to_toml(existing, &RuleAddition::MaxFileLines(500)).unwrap();
        let r = rules_table(&out);
        assert_eq!(r["max_file_lines"].as_integer(), Some(500));
    }

    #[test]
    fn rejects_zero_or_negative_line_limit() {
        assert!(add_rule_to_toml("", &RuleAddition::MaxFileLines(0)).is_err());
        assert!(add_rule_to_toml("", &RuleAddition::MaxFileLines(-5)).is_err());
    }

    #[test]
    fn rejects_empty_forbidden_sides() {
        let r = add_rule_to_toml(
            "",
            &RuleAddition::ForbiddenImport {
                from: "".into(),
                to: "db".into(),
            },
        );
        assert!(r.is_err());
    }

    // ---------- removal tests ----------

    #[test]
    fn removes_max_file_lines() {
        let existing = "[rules]\nmax_file_lines = 500\nno_cycles = true\n";
        let (out, outcome) =
            remove_rule_from_toml(existing, &RuleRemoval::MaxFileLines).unwrap();
        assert_eq!(outcome, RemovalOutcome::Removed);
        let r = rules_table(&out);
        assert!(r.get("max_file_lines").is_none());
        // Other rules unaffected.
        assert_eq!(r["no_cycles"].as_bool(), Some(true));
    }

    #[test]
    fn removes_no_cycles() {
        let existing = "[rules]\nno_cycles = true\nmax_file_lines = 500\n";
        let (out, outcome) = remove_rule_from_toml(existing, &RuleRemoval::NoCycles).unwrap();
        assert_eq!(outcome, RemovalOutcome::Removed);
        let r = rules_table(&out);
        assert!(r.get("no_cycles").is_none());
        assert_eq!(r["max_file_lines"].as_integer(), Some(500));
    }

    #[test]
    fn removes_specific_forbidden_import_only() {
        let existing = r#"
            [rules]
            forbidden_imports = ["ui -> db", "api -> secret", "tests -> prod"]
        "#;
        let (out, outcome) = remove_rule_from_toml(
            existing,
            &RuleRemoval::ForbiddenImport {
                from: "api".into(),
                to: "secret".into(),
            },
        )
        .unwrap();
        assert_eq!(outcome, RemovalOutcome::Removed);
        let r = rules_table(&out);
        let arr = r["forbidden_imports"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0].as_str(), Some("ui -> db"));
        assert_eq!(arr[1].as_str(), Some("tests -> prod"));
    }

    #[test]
    fn removing_last_forbidden_import_drops_the_key() {
        let existing = "[rules]\nforbidden_imports = [\"ui -> db\"]\n";
        let (out, outcome) = remove_rule_from_toml(
            existing,
            &RuleRemoval::ForbiddenImport {
                from: "ui".into(),
                to: "db".into(),
            },
        )
        .unwrap();
        assert_eq!(outcome, RemovalOutcome::Removed);
        let parsed: Table = out.parse().unwrap();
        let rules = parsed
            .get("rules")
            .and_then(|v| v.as_table())
            .expect("rules table still present");
        assert!(
            rules.get("forbidden_imports").is_none(),
            "empty forbidden_imports array should be cleaned up"
        );
    }

    #[test]
    fn removing_forbidden_import_is_whitespace_insensitive() {
        let existing = "[rules]\nforbidden_imports = [\"ui->db\"]\n"; // no spaces
        let (_out, outcome) = remove_rule_from_toml(
            existing,
            &RuleRemoval::ForbiddenImport {
                from: "ui".into(),
                to: "db".into(),
            },
        )
        .unwrap();
        assert_eq!(outcome, RemovalOutcome::Removed);
    }

    #[test]
    fn removing_nonexistent_scalar_is_noop() {
        let existing = "[rules]\nno_cycles = true\n";
        let (out, outcome) =
            remove_rule_from_toml(existing, &RuleRemoval::MaxFileLines).unwrap();
        assert_eq!(outcome, RemovalOutcome::NotPresent);
        // Existing rule untouched.
        let r = rules_table(&out);
        assert_eq!(r["no_cycles"].as_bool(), Some(true));
    }

    #[test]
    fn removing_nonexistent_forbidden_import_is_noop() {
        let existing = "[rules]\nforbidden_imports = [\"ui -> db\"]\n";
        let (out, outcome) = remove_rule_from_toml(
            existing,
            &RuleRemoval::ForbiddenImport {
                from: "nope".into(),
                to: "missing".into(),
            },
        )
        .unwrap();
        assert_eq!(outcome, RemovalOutcome::NotPresent);
        // The other rule survived.
        let r = rules_table(&out);
        assert_eq!(
            r["forbidden_imports"].as_array().unwrap()[0].as_str(),
            Some("ui -> db")
        );
    }

    #[test]
    fn removing_from_empty_file_is_noop() {
        let (out, outcome) = remove_rule_from_toml("", &RuleRemoval::NoCycles).unwrap();
        assert_eq!(outcome, RemovalOutcome::NotPresent);
        assert_eq!(out, "");
    }

    #[test]
    fn removing_from_file_with_no_rules_section_is_noop() {
        let existing = "[other]\nfoo = 1\n";
        let (out, outcome) =
            remove_rule_from_toml(existing, &RuleRemoval::MaxFileLines).unwrap();
        assert_eq!(outcome, RemovalOutcome::NotPresent);
        let parsed: Table = out.parse().unwrap();
        // Unrelated section preserved.
        assert_eq!(parsed["other"]["foo"].as_integer(), Some(1));
    }

    #[test]
    fn add_then_remove_round_trip() {
        // Add a rule, then remove it, the file should be back to where we started.
        let original = "[rules]\nmax_file_lines = 500\n";
        let added = add_rule_to_toml(
            original,
            &RuleAddition::ForbiddenImport {
                from: "ui".into(),
                to: "db".into(),
            },
        )
        .unwrap();
        let (removed, outcome) = remove_rule_from_toml(
            &added,
            &RuleRemoval::ForbiddenImport {
                from: "ui".into(),
                to: "db".into(),
            },
        )
        .unwrap();
        assert_eq!(outcome, RemovalOutcome::Removed);
        // The scalar rule is still there.
        let r = rules_table(&removed);
        assert_eq!(r["max_file_lines"].as_integer(), Some(500));
        assert!(r.get("forbidden_imports").is_none());
    }

    #[test]
    fn output_round_trips_through_rules_loader() {
        // Make sure whatever we write can be read back by the existing config loader.
        use crate::rules::Rules;
        let out = add_rule_to_toml(
            "",
            &RuleAddition::ForbiddenImport {
                from: "ui".into(),
                to: "db".into(),
            },
        )
        .unwrap();
        let out = add_rule_to_toml(&out, &RuleAddition::MaxFileLines(500)).unwrap();
        let out = add_rule_to_toml(&out, &RuleAddition::NoCycles(true)).unwrap();
        let parsed = Rules::from_toml_str(&out).unwrap();
        assert_eq!(parsed.max_file_lines, Some(500));
        assert!(parsed.no_cycles);
        assert_eq!(parsed.forbidden_imports.len(), 1);
        assert_eq!(parsed.forbidden_imports[0].from, "ui");
        assert_eq!(parsed.forbidden_imports[0].to, "db");
    }
}
