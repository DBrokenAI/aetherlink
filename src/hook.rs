//! `aetherlink --hook-check` — Claude Code `PreToolUse` hook entry point.
//!
//! Why this exists: Claude Code's MCP tools are *opt-in*. The agent calls
//! `apply_guarded_change` only if it decides to. In practice it usually
//! reaches for the built-in `Edit` / `Write` / `MultiEdit` tools and the
//! AetherLink rules never fire. That makes AetherLink a *suggestion*, not a
//! guardrail.
//!
//! Hooks fix that. A `PreToolUse` hook is invoked by the Claude Code harness
//! *before* the tool actually runs. The hook reads the tool name and arguments
//! from stdin (JSON), decides allow/block, and signals back via exit code:
//!
//!   * exit 0  → allow the tool call to proceed
//!   * exit 2  → block the tool call. The text on stderr is fed back to
//!               Claude as the rejection reason.
//!
//! With this hook installed, every Edit/Write/MultiEdit on a file inside an
//! AetherLink-managed project is run through the validator BEFORE the write
//! happens, regardless of whether the agent knows AetherLink exists. The
//! agent literally cannot route around it. That is the whole product.
//!
//! Project detection: we walk up from the target file path looking for
//! `AetherLink.toml`. If we don't find one, the hook allows the write
//! unconditionally — projects without AetherLink rules are not our problem.

use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde_json::Value;

use crate::graph::DependencyGraph;
use crate::rules::{
    baseline::split_against_baseline, validate_with_overrides, Baseline, RuleViolation, RulesFile,
    Severity,
};
use crate::scanner::{FileScanner, ImportParser, Language, ScannedFile};

/// Entry point. Reads a JSON tool-call payload from stdin, decides whether to
/// allow or block, and exits with the appropriate code. Never panics on
/// malformed input — anything we can't understand is allowed through, because
/// the alternative (crashing the harness on every write) is much worse than
/// failing open on the rare malformed payload.
pub fn run() -> Result<()> {
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .context("reading hook payload from stdin")?;

    let payload: Value = match serde_json::from_str(&buf) {
        Ok(v) => v,
        Err(_) => {
            // Fail open: if we can't parse, don't block the user.
            std::process::exit(0);
        }
    };

    let tool_name = payload["tool_name"].as_str().unwrap_or("");
    let tool_input = &payload["tool_input"];

    let proposed = match extract_proposed_write(tool_name, tool_input) {
        Ok(Some(p)) => p,
        Ok(None) => {
            // Not a write-shaped tool call — nothing for us to validate.
            std::process::exit(0);
        }
        Err(e) => {
            // Couldn't reconstruct the post-write state. Surface the error
            // but fail open: better to let one edit through than to brick
            // the agent loop on a malformed Edit payload.
            eprintln!("aetherlink hook: could not interpret {tool_name} call: {e}");
            std::process::exit(0);
        }
    };

    // Find the AetherLink project root by walking up from the target file.
    // No project = no rules = allow.
    let Some(project_root) = find_project_root(&proposed.file_path) else {
        std::process::exit(0);
    };

    // Bypass file: if `.aetherlink_bypass` exists in the project root, the
    // user has explicitly opted out of enforcement. Allow with a stderr note
    // so they don't forget the bypass is on.
    if project_root.join(".aetherlink_bypass").exists() {
        eprintln!(
            "aetherlink hook: bypass file present in {}, allowing write",
            project_root.display()
        );
        std::process::exit(0);
    }

    match evaluate(&project_root, &proposed) {
        Ok(violations) if violations.is_empty() => std::process::exit(0),
        Ok(violations) => {
            // Print a structured rejection message to stderr. Claude Code
            // forwards this back to the model on exit code 2.
            eprintln!("BLOCKED by AetherLink (project: {}).", project_root.display());
            eprintln!(
                "{} architectural rule violation{} would be introduced:",
                violations.len(),
                if violations.len() == 1 { "" } else { "s" }
            );
            for (i, v) in violations.iter().enumerate() {
                eprintln!("  [{}] {}: {}", i + 1, v.rule, v.message);
            }
            eprintln!(
                "Fix the issues above and retry, or use apply_guarded_change \
                 (which runs the same checks atomically). To temporarily \
                 disable enforcement, create .aetherlink_bypass in the project \
                 root."
            );
            std::process::exit(2);
        }
        Err(e) => {
            // Internal error — fail open with a note.
            eprintln!("aetherlink hook: internal error, allowing write: {e}");
            std::process::exit(0);
        }
    }
}

/// What we need to know to validate a proposed write: the absolute path of
/// the target file and the *full post-write content* of that file. For Write
/// this is given directly; for Edit/MultiEdit we have to apply the patch to
/// the existing file ourselves.
struct ProposedWrite {
    file_path: PathBuf,
    new_content: String,
}

fn extract_proposed_write(tool_name: &str, input: &Value) -> Result<Option<ProposedWrite>> {
    match tool_name {
        "Write" => {
            let file_path = input["file_path"]
                .as_str()
                .ok_or_else(|| anyhow!("Write missing file_path"))?;
            let content = input["content"].as_str().unwrap_or("").to_string();
            Ok(Some(ProposedWrite {
                file_path: PathBuf::from(file_path),
                new_content: content,
            }))
        }
        "Edit" => {
            let file_path = input["file_path"]
                .as_str()
                .ok_or_else(|| anyhow!("Edit missing file_path"))?;
            let old_string = input["old_string"].as_str().unwrap_or("");
            let new_string = input["new_string"].as_str().unwrap_or("");
            let replace_all = input["replace_all"].as_bool().unwrap_or(false);

            // Read the existing file. If it doesn't exist this isn't a real
            // Edit (the harness would reject it), so report nothing to check.
            let existing = std::fs::read_to_string(file_path).unwrap_or_default();
            let new_content = if replace_all {
                existing.replace(old_string, new_string)
            } else {
                existing.replacen(old_string, new_string, 1)
            };
            Ok(Some(ProposedWrite {
                file_path: PathBuf::from(file_path),
                new_content,
            }))
        }
        "MultiEdit" | "NotebookEdit" => {
            // MultiEdit applies a sequence of edits to the same file.
            let file_path = input["file_path"]
                .as_str()
                .ok_or_else(|| anyhow!("MultiEdit missing file_path"))?;
            let edits = input["edits"]
                .as_array()
                .ok_or_else(|| anyhow!("MultiEdit missing edits[]"))?;
            let mut content = std::fs::read_to_string(file_path).unwrap_or_default();
            for e in edits {
                let old_s = e["old_string"].as_str().unwrap_or("");
                let new_s = e["new_string"].as_str().unwrap_or("");
                let replace_all = e["replace_all"].as_bool().unwrap_or(false);
                content = if replace_all {
                    content.replace(old_s, new_s)
                } else {
                    content.replacen(old_s, new_s, 1)
                };
            }
            Ok(Some(ProposedWrite {
                file_path: PathBuf::from(file_path),
                new_content: content,
            }))
        }
        _ => Ok(None),
    }
}

/// Walk up from a file path looking for an `AetherLink.toml`. Returns the
/// directory containing it (i.e. the project root) if found.
fn find_project_root(file_path: &Path) -> Option<PathBuf> {
    let mut cur = if file_path.is_absolute() {
        file_path.parent()?.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(file_path).parent()?.to_path_buf()
    };
    loop {
        if cur.join("AetherLink.toml").exists() {
            return Some(cur);
        }
        cur = cur.parent()?.to_path_buf();
    }
}

/// Run the same validation pipeline `apply_guarded_change` uses, but driven
/// from a hook payload instead of an MCP request. Returns the list of
/// *blocking* violations (after baseline, after severity demotion, after the
/// per-target ratchet).
fn evaluate(project_root: &Path, proposed: &ProposedWrite) -> Result<Vec<RuleViolation>> {
    let ext = proposed
        .file_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    let language = Language::from_extension(ext);
    if language == Language::Unknown {
        // Files we don't parse can't break import/cycle rules and the line
        // limit alone isn't worth blocking on for unknown extensions.
        return Ok(Vec::new());
    }

    let mut parser = ImportParser::new()?;
    let synthetic = ScannedFile {
        path: proposed.file_path.clone(),
        language: language.clone(),
        imports: parser.parse(&proposed.new_content, &language),
        exports: Vec::new(),
        line_count: proposed.new_content.lines().count(),
    };

    // Build the post-write project view.
    let scanner = FileScanner::new(project_root);
    let mut files = scanner.scan()?;
    let mut replaced = false;
    for f in files.iter_mut() {
        if f.path == proposed.file_path {
            *f = synthetic.clone();
            replaced = true;
            break;
        }
    }
    if !replaced {
        files.push(synthetic.clone());
    }

    let rules_file = RulesFile::load(project_root)?;
    let mut graph = DependencyGraph::new(project_root);
    graph.build(&files);
    let raw_after =
        validate_with_overrides(&files, &graph, &rules_file, Some(project_root));

    // Same ratchet logic as the MCP server: filter against committed baseline,
    // filter against on-disk pre-existing violations (only block if new or
    // touches the target file), and demote warn-severity rules.
    let baseline = Baseline::load(project_root)?;
    let (after_baseline, _grand) = split_against_baseline(raw_after, baseline.as_ref());

    let on_disk = FileScanner::new(project_root).scan()?;
    let mut on_disk_graph = DependencyGraph::new(project_root);
    on_disk_graph.build(&on_disk);
    let pre_existing =
        validate_with_overrides(&on_disk, &on_disk_graph, &rules_file, Some(project_root));
    let pre_fps: std::collections::HashSet<String> =
        pre_existing.iter().map(|v| v.fingerprint()).collect();

    let target_str = proposed.file_path.display().to_string();
    let mut blocking = Vec::new();
    for v in after_baseline {
        if v.severity == Severity::Warning {
            continue;
        }
        let pre = pre_fps.contains(&v.fingerprint());
        let touches_target = v.message.contains(&target_str);
        if !pre || touches_target {
            blocking.push(v);
        }
    }
    Ok(blocking)
}
