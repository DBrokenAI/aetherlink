//! `aetherlink --add` — interactive rule manager.
//!
//! Top-level menu lets the user pick **Add a rule** or **Remove a rule**, then
//! walks them through the appropriate prompts. Either flow ends with an
//! immediate scan so the tray icon reflects the new state.

use std::env;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use dialoguer::theme::ColorfulTheme;
use dialoguer::{Confirm, Input, Select};

use crate::graph::DependencyGraph;
use crate::rules::{
    add_rule_to_toml, remove_rule_from_toml, validate, RemovalOutcome, RuleAddition, RuleRemoval,
    Rules,
};
use crate::scanner::FileScanner;

const ACTIONS: &[&str] = &["Add a rule", "Remove a rule"];

const RULE_TYPES: &[&str] = &[
    "Forbidden Import (block one folder from importing another)",
    "Line Limit (cap how big a single file can get)",
    "No Cycles (block circular dependencies)",
];

pub fn run() -> Result<()> {
    println!("AetherLink — Rule Manager");
    println!("=========================");

    let project_root = env::current_dir().context("reading current directory")?;
    println!("Project root: {}", project_root.display());
    println!();

    let theme = ColorfulTheme::default();

    let action = Select::with_theme(&theme)
        .with_prompt("What do you want to do?")
        .items(ACTIONS)
        .default(0)
        .interact()?;

    match action {
        0 => add_flow(&theme, &project_root),
        1 => remove_flow(&theme, &project_root),
        _ => unreachable!(),
    }
}

fn add_flow(theme: &ColorfulTheme, project_root: &Path) -> Result<()> {
    let choice = Select::with_theme(theme)
        .with_prompt("What kind of rule?")
        .items(RULE_TYPES)
        .default(0)
        .interact()?;

    let addition = match choice {
        0 => prompt_forbidden_import(theme)?,
        1 => prompt_line_limit(theme)?,
        2 => prompt_no_cycles(theme)?,
        _ => unreachable!(),
    };

    let toml_path = project_root.join("AetherLink.toml");
    let existing = read_or_empty(&toml_path)?;
    let updated = add_rule_to_toml(&existing, &addition)?;
    fs::write(&toml_path, &updated)
        .with_context(|| format!("writing {}", toml_path.display()))?;

    println!();
    println!("Done! Rule saved.");
    println!("  {}", addition.human_label());
    println!("  -> {}", toml_path.display());
    println!();

    println!("Re-scanning project against the new rules...");
    print_scan_summary(project_root)?;
    Ok(())
}

fn remove_flow(theme: &ColorfulTheme, project_root: &Path) -> Result<()> {
    let toml_path = project_root.join("AetherLink.toml");
    if !toml_path.exists() {
        println!("No AetherLink.toml in this folder. Nothing to remove.");
        return Ok(());
    }

    let existing = fs::read_to_string(&toml_path)
        .with_context(|| format!("reading {}", toml_path.display()))?;
    let rules = Rules::from_toml_str(&existing)
        .with_context(|| format!("parsing {}", toml_path.display()))?;

    // Build a list of currently-set rules the user can pick from.
    let mut options: Vec<(String, RuleRemoval)> = Vec::new();
    if let Some(n) = rules.max_file_lines {
        options.push((
            format!("max_file_lines = {n}"),
            RuleRemoval::MaxFileLines,
        ));
    }
    if rules.no_cycles {
        options.push(("no_cycles = true".to_string(), RuleRemoval::NoCycles));
    }
    for fi in &rules.forbidden_imports {
        options.push((
            format!("forbidden_import: {} -> {}", fi.from, fi.to),
            RuleRemoval::ForbiddenImport {
                from: fi.from.clone(),
                to: fi.to.clone(),
            },
        ));
    }

    if options.is_empty() {
        println!("No rules currently set in AetherLink.toml. Nothing to remove.");
        return Ok(());
    }

    let labels: Vec<&str> = options.iter().map(|(l, _)| l.as_str()).collect();
    let pick = Select::with_theme(theme)
        .with_prompt("Which rule should I remove?")
        .items(&labels)
        .default(0)
        .interact()?;

    let confirmed = Confirm::with_theme(theme)
        .with_prompt(format!("Remove '{}'?", labels[pick]))
        .default(true)
        .interact()?;
    if !confirmed {
        println!("Cancelled. No changes made.");
        return Ok(());
    }

    let (label, removal) = &options[pick];
    let (updated, outcome) = remove_rule_from_toml(&existing, removal)?;

    match outcome {
        RemovalOutcome::Removed => {
            fs::write(&toml_path, &updated)
                .with_context(|| format!("writing {}", toml_path.display()))?;
            println!();
            println!("Done! Removed: {label}");
            println!("  -> {}", toml_path.display());
        }
        RemovalOutcome::NotPresent => {
            // Should be unreachable since we built `options` from the parsed
            // file moments ago, but report it cleanly if it ever happens.
            println!();
            println!("(internal) Rule '{label}' wasn't present after re-parse. No changes made.");
        }
    }

    println!();
    println!("Re-scanning project against the updated rules...");
    print_scan_summary(project_root)?;
    Ok(())
}

fn read_or_empty(path: &Path) -> Result<String> {
    if path.exists() {
        fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))
    } else {
        Ok(String::new())
    }
}

fn prompt_forbidden_import(theme: &ColorfulTheme) -> Result<RuleAddition> {
    let to: String = Input::with_theme(theme)
        .with_prompt("Which folder is restricted? (e.g. 'db' or 'src/db')")
        .interact_text()?;
    let from: String = Input::with_theme(theme)
        .with_prompt("Which folder cannot touch it? (e.g. 'ui' or 'src/ui')")
        .interact_text()?;
    let to = last_segment(&to);
    let from = last_segment(&from);
    Ok(RuleAddition::ForbiddenImport { from, to })
}

fn prompt_line_limit(theme: &ColorfulTheme) -> Result<RuleAddition> {
    let n: i64 = Input::with_theme(theme)
        .with_prompt("Maximum lines per file")
        .default(500)
        .interact_text()?;
    Ok(RuleAddition::MaxFileLines(n))
}

fn prompt_no_cycles(theme: &ColorfulTheme) -> Result<RuleAddition> {
    let enabled: bool = Confirm::with_theme(theme)
        .with_prompt("Block circular dependencies in the import graph?")
        .default(true)
        .interact()?;
    Ok(RuleAddition::NoCycles(enabled))
}

/// Take a slash- or backslash-separated path like `src/db` and keep just the
/// final segment (`db`). The forbidden_imports rule matches on directory
/// component names, so passing the full prefix would never match anything.
fn last_segment(input: &str) -> String {
    input
        .trim()
        .trim_end_matches(['/', '\\'])
        .rsplit(|c| c == '/' || c == '\\')
        .next()
        .unwrap_or("")
        .to_string()
}

fn print_scan_summary(project_root: &Path) -> Result<()> {
    let scanner = FileScanner::new(project_root);
    let files = scanner.scan()?;
    let mut graph = DependencyGraph::new(project_root);
    graph.build(&files);
    let rules = Rules::load(project_root)?;
    let violations = validate(&files, &graph, &rules);

    println!(
        "Scanned {} files, {} graph edges.",
        files.len(),
        graph.edge_count()
    );

    if violations.is_empty() {
        println!("LEGAL: project passes all rules.");
        return Ok(());
    }

    println!();
    println!(
        "WARNING: project currently has {} violation{} of the rule set:",
        violations.len(),
        if violations.len() == 1 { "" } else { "s" }
    );
    for (i, v) in violations.iter().enumerate() {
        println!("  [{}] {} — {}", i + 1, v.rule, v.message);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_segment_strips_path_prefix() {
        assert_eq!(last_segment("src/db"), "db");
        assert_eq!(last_segment("src\\db"), "db");
        assert_eq!(last_segment("db"), "db");
        assert_eq!(last_segment("src/db/"), "db");
        assert_eq!(last_segment("  src/ui  "), "ui");
    }
}
