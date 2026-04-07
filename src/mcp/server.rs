use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use serde_json::{json, Value};
use tokio::io::{self, AsyncBufReadExt, AsyncWriteExt, BufReader};

use std::time::Duration;

use super::lease::{AcquireResult, LeaseManager, ReleaseResult, LEASE_DURATION};
use crate::graph::DependencyGraph;
use crate::notify;
use crate::rules::{
    add_rule_to_toml, baseline::split_against_baseline, remove_rule_from_toml,
    validate_with_overrides, Baseline, RemovalOutcome, RuleAddition, RuleRemoval, RuleViolation,
    Rules, RulesFile, Severity,
};
use crate::scanner::{FileScanner, ImportParser, Language, ScannedFile};
use crate::security;
use crate::status::{self, State, Status, ViolationSummary};

/// MCP server that communicates over stdio using JSON-RPC.
pub struct McpServer {
    leases: Mutex<LeaseManager>,
}

/// Output of `evaluate_proposed_change`. Both `check_change` and
/// `apply_guarded_change` consume this struct so the validation path is
/// guaranteed identical between the two tools.
///
/// `violations` are the *blocking* violations: new errors introduced (or
/// preserved by touching the target file) that should reject the write.
/// `warnings` are non-blocking violations: either explicitly `severity =
/// "warn"` rules, or pre-existing rot grandfathered in by the baseline file.
/// We surface them in reports so the user can see the debt without being
/// forced to fix it before unrelated edits.
struct ProposedChange {
    target: PathBuf,
    synthetic: ScannedFile,
    violations: Vec<RuleViolation>,
    warnings: Vec<RuleViolation>,
    rules: Rules,
}

/// Outcome of the path-sandbox check. `Blocked` carries a pre-formatted
/// user-facing message so callers can return it directly to MCP.
enum SandboxResult {
    Ok(PathBuf),
    Blocked(String),
}

impl McpServer {
    pub fn new() -> Self {
        Self {
            leases: Mutex::new(LeaseManager::new()),
        }
    }

    pub async fn run(&self) -> Result<()> {
        let stdin = io::stdin();
        let mut stdout = io::stdout();
        let mut reader = BufReader::new(stdin);
        let mut line = String::new();

        tracing::info!("AetherLink MCP server starting on stdio");

        loop {
            line.clear();
            let bytes_read = reader.read_line(&mut line).await?;
            if bytes_read == 0 {
                break; // EOF
            }

            let request: Value = match serde_json::from_str(line.trim()) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("Invalid JSON: {e}");
                    continue;
                }
            };

            let response = self.handle_request(&request).await;

            let mut out = serde_json::to_string(&response)?;
            out.push('\n');
            stdout.write_all(out.as_bytes()).await?;
            stdout.flush().await?;
        }

        Ok(())
    }

    async fn handle_request(&self, request: &Value) -> Value {
        let method = request["method"].as_str().unwrap_or("");
        let id = &request["id"];

        match method {
            "initialize" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {
                        "tools": {}
                    },
                    "serverInfo": {
                        "name": "aetherlink",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }
            }),

            "tools/list" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "tools": [
                        {
                            "name": "scan_project",
                            "description": "Scan the project directory and build a dependency graph",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "path": {
                                        "type": "string",
                                        "description": "Root directory of the project to scan"
                                    }
                                },
                                "required": ["path"]
                            }
                        },
                        {
                            "name": "acquire_lease",
                            "description": "Acquire an exclusive 5-minute editing lease on a file. Returns 'Access Denied' if another agent already holds the lease. Use this before modifying any file to prevent merge conflicts between concurrent agents.",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "file_path": {
                                        "type": "string",
                                        "description": "Absolute path of the file you intend to modify"
                                    }
                                },
                                "required": ["file_path"]
                            }
                        },
                        {
                            "name": "release_lease",
                            "description": "Release an active editing lease early so other agents don't have to wait for the 5-minute timeout. Call this as soon as you finish editing a file.",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "file_path": {
                                        "type": "string",
                                        "description": "Absolute path of the file whose lease should be released"
                                    }
                                },
                                "required": ["file_path"]
                            }
                        },
                        {
                            "name": "add_rule",
                            "description": "Add a new architectural rule to the project's AetherLink.toml and immediately re-scan to surface any code that already violates it. Use this when the user says 'add a rule for X' or 'enforce X'. Supported rule_type values: 'forbidden_import' (params: {from, to} OR {rule: 'ui -> db'}), 'line_limit' / 'max_file_lines' (params: {value: 500}), 'no_cycles' (params: {enabled: true}). The AetherLink.toml is created if it does not exist; existing rules are preserved.",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "project_path": {
                                        "type": "string",
                                        "description": "Root directory of the project where AetherLink.toml lives or should be created"
                                    },
                                    "rule_type": {
                                        "type": "string",
                                        "description": "One of: forbidden_import, line_limit, max_file_lines, no_cycles"
                                    },
                                    "params": {
                                        "type": "object",
                                        "description": "Rule-specific parameters. forbidden_import: {from, to} or {rule}. line_limit: {value}. no_cycles: {enabled}."
                                    }
                                },
                                "required": ["project_path", "rule_type", "params"]
                            }
                        },
                        {
                            "name": "remove_rule",
                            "description": "Remove an architectural rule from AetherLink.toml and immediately re-scan so the tray icon flips back to green if the project is now clean. Use this when the user says 'remove the X rule', 'stop enforcing X', or 'allow X again'. Supported rule_type values: 'forbidden_import' (params: {from, to} OR {rule: 'ui -> db'}) — removes only the matching entry from the array, leaving other forbidden imports alone; 'line_limit' / 'max_file_lines' — drops the line entirely (params can be empty {}); 'no_cycles' — drops the line entirely. Removing a rule that isn't set is a safe no-op.",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "project_path": {
                                        "type": "string",
                                        "description": "Root directory of the project where AetherLink.toml lives"
                                    },
                                    "rule_type": {
                                        "type": "string",
                                        "description": "One of: forbidden_import, line_limit, max_file_lines, no_cycles"
                                    },
                                    "params": {
                                        "type": "object",
                                        "description": "For forbidden_import: {from, to} or {rule}. For line_limit and no_cycles: {} (no params needed)."
                                    }
                                },
                                "required": ["project_path", "rule_type", "params"]
                            }
                        },
                        {
                            "name": "apply_guarded_change",
                            "description": "**THIS IS THE ONLY WAY TO SAVE FILES IN A PROJECT GUARDED BY AETHERLINK.** Validate a proposed file edit and, IF AND ONLY IF every architectural rule passes, write it to disk atomically in the same call. There is no gap between the check and the write — a stale check_change result cannot be silently bypassed. Pass the project root, the file path being changed, and the full proposed new contents. Returns either 'APPLIED' (the file was written) or 'WRITE BLOCKED' (the file was NOT written and the on-disk project is unchanged). DO NOT use any other file-writing tool to modify files in this project — bypass attempts leave the project in an inconsistent state and defeat the entire purpose of AetherLink.",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "project_path": {
                                        "type": "string",
                                        "description": "Root directory of the project (where AetherLink.toml lives)"
                                    },
                                    "file_path": {
                                        "type": "string",
                                        "description": "Absolute path of the file to write"
                                    },
                                    "new_content": {
                                        "type": "string",
                                        "description": "The full proposed new contents of the file"
                                    }
                                },
                                "required": ["project_path", "file_path", "new_content"]
                            }
                        },
                        {
                            "name": "check_change",
                            "description": "Dry-run validator for a proposed file edit — does NOT write to disk. Useful when an agent wants to ask 'would this be legal?' without committing. To actually save a file, use `apply_guarded_change` instead, which validates and writes atomically. Pass the project root, file path, and full proposed contents. Returns APPROVED or WRITE BLOCKED with violations.",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "project_path": {
                                        "type": "string",
                                        "description": "Root directory of the project (where AetherLink.toml lives)"
                                    },
                                    "file_path": {
                                        "type": "string",
                                        "description": "Absolute path of the file being created or modified"
                                    },
                                    "new_content": {
                                        "type": "string",
                                        "description": "The full proposed new contents of the file"
                                    }
                                },
                                "required": ["project_path", "file_path", "new_content"]
                            }
                        }
                    ]
                }
            }),

            "tools/call" => {
                let tool_name = request["params"]["name"].as_str().unwrap_or("");
                let args = &request["params"]["arguments"];
                match tool_name {
                    "acquire_lease" => match self.run_acquire_lease(args) {
                        Ok(text) => json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "content": [{ "type": "text", "text": text }]
                            }
                        }),
                        Err(e) => self.error_response(id, -32602, &format!("acquire_lease failed: {e}")),
                    },
                    "release_lease" => match self.run_release_lease(args) {
                        Ok(text) => json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "content": [{ "type": "text", "text": text }]
                            }
                        }),
                        Err(e) => self.error_response(id, -32602, &format!("release_lease failed: {e}")),
                    },
                    "scan_project" => match Self::run_scan(args) {
                        Ok(text) => json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "content": [{ "type": "text", "text": text }]
                            }
                        }),
                        Err(e) => self.error_response(id, -32000, &format!("scan failed: {e}")),
                    },
                    "add_rule" => match Self::run_add_rule(args) {
                        Ok(text) => json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "content": [{ "type": "text", "text": text }]
                            }
                        }),
                        Err(e) => self.error_response(id, -32000, &format!("add_rule failed: {e}")),
                    },
                    "remove_rule" => match Self::run_remove_rule(args) {
                        Ok(text) => json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "content": [{ "type": "text", "text": text }]
                            }
                        }),
                        Err(e) => self.error_response(id, -32000, &format!("remove_rule failed: {e}")),
                    },
                    "check_change" => match Self::run_check_change(args) {
                        Ok(text) => json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "content": [{ "type": "text", "text": text }]
                            }
                        }),
                        Err(e) => self.error_response(id, -32000, &format!("check_change failed: {e}")),
                    },
                    "apply_guarded_change" => match Self::run_apply_guarded_change(args) {
                        Ok(text) => json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "content": [{ "type": "text", "text": text }]
                            }
                        }),
                        Err(e) => self.error_response(id, -32000, &format!("apply_guarded_change failed: {e}")),
                    },
                    _ => self.error_response(id, -32601, "Unknown tool"),
                }
            }

            _ => self.error_response(id, -32601, "Method not found"),
        }
    }

    /// Acquire an exclusive editing lease on a file for `LEASE_DURATION`.
    /// Returns the user-facing message describing whether the lease was granted.
    fn run_acquire_lease(&self, args: &Value) -> Result<String> {
        let file_path = args["file_path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing 'file_path' argument"))?;
        let path = PathBuf::from(file_path);

        let mut mgr = self
            .leases
            .lock()
            .map_err(|_| anyhow::anyhow!("lease manager mutex poisoned"))?;

        match mgr.acquire(path.clone()) {
            AcquireResult::Granted { expires_in } => Ok(format!(
                "LEASE GRANTED: {} is now locked for editing for {} seconds (until {} from now). Release happens automatically on expiry.",
                path.display(),
                expires_in.as_secs(),
                format_duration(expires_in)
            )),
            AcquireResult::Denied { remaining } => Ok(format!(
                "Access Denied: File is currently being modified by another agent.\n\
                 File: {}\n\
                 Remaining lock time: {} (~{} seconds)\n\
                 Retry after the existing lease expires (max lease duration: {} seconds).",
                path.display(),
                format_duration(remaining),
                remaining.as_secs(),
                LEASE_DURATION.as_secs()
            )),
        }
    }

    /// Add a new architectural rule to the project's `AetherLink.toml` and
    /// immediately re-scan to surface any code that already violates it.
    fn run_add_rule(args: &Value) -> Result<String> {
        let project_path = args["project_path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing 'project_path' argument"))?;
        let rule_type = args["rule_type"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing 'rule_type' argument"))?;
        let params = &args["params"];

        let addition = parse_rule_addition(rule_type, params)?;

        let root = PathBuf::from(project_path);
        let toml_path = root.join("AetherLink.toml");

        let existing = if toml_path.exists() {
            fs::read_to_string(&toml_path)
                .with_context(|| format!("reading {}", toml_path.display()))?
        } else {
            String::new()
        };

        let updated = add_rule_to_toml(&existing, &addition)?;
        fs::write(&toml_path, &updated)
            .with_context(|| format!("writing {}", toml_path.display()))?;

        // Re-scan immediately so the user sees whether the new rule is already
        // being violated.
        let scan_args = json!({ "path": project_path });
        let scan_output = match Self::run_scan(&scan_args) {
            Ok(text) => text,
            Err(e) => format!("(re-scan failed: {e})"),
        };

        Ok(format!(
            "Rule added to AetherLink.toml. I am now enforcing this for you.\n\
             - Rule: {}\n\
             - File: {}\n\n\
             --- Updated AetherLink.toml ---\n{}\n\
             --- Immediate scan results ---\n{}",
            addition.human_label(),
            toml_path.display(),
            updated,
            scan_output
        ))
    }

    /// Remove a rule from `AetherLink.toml` and immediately re-scan so the
    /// tray reflects the new state. Removing a rule that isn't set is a safe
    /// no-op — we still re-scan and report the current status.
    fn run_remove_rule(args: &Value) -> Result<String> {
        let project_path = args["project_path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing 'project_path' argument"))?;
        let rule_type = args["rule_type"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing 'rule_type' argument"))?;
        let params = &args["params"];

        let removal = parse_rule_removal(rule_type, params)?;

        let root = PathBuf::from(project_path);
        let toml_path = root.join("AetherLink.toml");

        if !toml_path.exists() {
            // Still re-scan so the tray gets a fresh "legal/illegal" reading,
            // even though there's nothing to delete.
            let scan_args = json!({ "path": project_path });
            let scan_output = match Self::run_scan(&scan_args) {
                Ok(text) => text,
                Err(e) => format!("(re-scan failed: {e})"),
            };
            return Ok(format!(
                "No AetherLink.toml at {}. Nothing to remove.\n\n--- Re-scan results ---\n{}",
                toml_path.display(),
                scan_output
            ));
        }

        let existing = fs::read_to_string(&toml_path)
            .with_context(|| format!("reading {}", toml_path.display()))?;
        let (updated, outcome) = remove_rule_from_toml(&existing, &removal)?;

        let outcome_msg = match outcome {
            RemovalOutcome::Removed => {
                fs::write(&toml_path, &updated)
                    .with_context(|| format!("writing {}", toml_path.display()))?;
                format!("Rule removed: {}. I am no longer enforcing it.", removal.human_label())
            }
            RemovalOutcome::NotPresent => {
                format!(
                    "Rule '{}' was not set in AetherLink.toml — nothing to remove.",
                    removal.human_label()
                )
            }
        };

        // Always re-scan. This is the whole point: the user wants the tray to
        // flip from red to green the moment a blocking rule is dropped.
        let scan_args = json!({ "path": project_path });
        let scan_output = match Self::run_scan(&scan_args) {
            Ok(text) => text,
            Err(e) => format!("(re-scan failed: {e})"),
        };

        Ok(format!(
            "{}\n- File: {}\n\n--- Updated AetherLink.toml ---\n{}\n--- Re-scan results ---\n{}",
            outcome_msg,
            toml_path.display(),
            updated,
            scan_output
        ))
    }

    /// Release a previously acquired lease early, freeing the file for other agents.
    fn run_release_lease(&self, args: &Value) -> Result<String> {
        let file_path = args["file_path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing 'file_path' argument"))?;
        let path = PathBuf::from(file_path);

        let mut mgr = self
            .leases
            .lock()
            .map_err(|_| anyhow::anyhow!("lease manager mutex poisoned"))?;

        Ok(match mgr.release(&path) {
            ReleaseResult::Released => format!(
                "LEASE RELEASED: {} is now Open. Other agents can acquire it immediately.",
                path.display()
            ),
            ReleaseResult::AlreadyExpired => format!(
                "Lease on {} had already expired and was cleaned up. The file was already Open.",
                path.display()
            ),
            ReleaseResult::NotHeld => format!(
                "No active lease on {} — nothing to release.",
                path.display()
            ),
        })
    }

    /// Result of running the rule engine against a hypothetical post-edit
    /// state of the project. Used by both `check_change` (which only reports)
    /// and `apply_guarded_change` (which writes if violations is empty).
    fn evaluate_proposed_change(
        root: &Path,
        target: PathBuf,
        new_content: &str,
        language: Language,
    ) -> Result<ProposedChange> {
        // Build the synthetic file representing the proposed state.
        let mut parser = ImportParser::new()?;
        let synthetic = ScannedFile {
            path: target.clone(),
            language: language.clone(),
            imports: parser.parse(new_content, &language),
            exports: Vec::new(),
            line_count: new_content.lines().count(),
        };

        // Scan the existing project, then patch in the synthetic file. If the
        // target file already exists in the scan, we replace it; otherwise we
        // append, so this works for both edits and brand-new files.
        let scanner = FileScanner::new(root);
        let mut files = scanner.scan()?;
        let mut replaced = false;
        for f in files.iter_mut() {
            if f.path == target {
                *f = synthetic.clone();
                replaced = true;
                break;
            }
        }
        if !replaced {
            files.push(synthetic.clone());
        }

        // Build a graph and validate against the project rules. Validation
        // uses `RulesFile` so per-folder `[[overrides]]` are honored.
        let mut graph = DependencyGraph::new(root);
        graph.build(&files);
        let rules_file = RulesFile::load(root)?;
        let rules = rules_file.rules.clone();
        let violations_after =
            validate_with_overrides(&files, &graph, &rules_file, Some(root));

        // === Ratchet logic ===
        //
        // We layer two filters on top of the raw "after" violations to decide
        // what is *blocking* vs. what is just informational:
        //
        //  1. The committed `.aetherlink-baseline.json` (if present). Any
        //     violation whose fingerprint is in the baseline is grandfathered:
        //     it's pre-existing rot, the user has acknowledged it, and it
        //     does not block. This replaces the brittle "filter by message
        //     text" heuristic the previous version used.
        //
        //  2. The dynamic baseline computed from the *current on-disk* state.
        //     Even without a committed baseline, we don't want unrelated
        //     pre-existing violations to block this specific write. Any
        //     violation that already existed before the proposed edit, *and*
        //     doesn't mention the file being written, is also informational.
        //     Touching a still-broken target file remains blocking — that's
        //     how we keep "you must fix it as you touch it" semantics for
        //     opportunistic cleanup.
        //
        //  3. `severity = "warn"` rules never block regardless of either
        //     baseline. They go straight to the warnings bucket.
        let committed_baseline = Baseline::load(root)?;
        let (after_baseline_filter, mut warnings) =
            split_against_baseline(violations_after, committed_baseline.as_ref());

        let on_disk_files = FileScanner::new(root).scan()?;
        let mut on_disk_graph = DependencyGraph::new(root);
        on_disk_graph.build(&on_disk_files);
        let pre_existing = validate_with_overrides(
            &on_disk_files,
            &on_disk_graph,
            &rules_file,
            Some(root),
        );
        let pre_existing_fps: std::collections::HashSet<String> =
            pre_existing.iter().map(|v| v.fingerprint()).collect();

        let target_str = target.display().to_string();
        let mut violations = Vec::new();
        for v in after_baseline_filter {
            // Warnings never block.
            if v.severity == Severity::Warning {
                warnings.push(v);
                continue;
            }
            let pre = pre_existing_fps.contains(&v.fingerprint());
            let touches_target = v.message.contains(&target_str);
            if !pre || touches_target {
                violations.push(v);
            } else {
                warnings.push(v);
            }
        }

        Ok(ProposedChange {
            target,
            synthetic,
            violations,
            warnings,
            rules,
        })
    }

    /// Common arg extraction for `check_change` and `apply_guarded_change`.
    /// Returns `(root, target, new_content, language)` or, on unsupported
    /// extensions, an `Err` with a user-facing message.
    fn extract_change_args(args: &Value) -> Result<(PathBuf, PathBuf, String, Language)> {
        let project_path = args["project_path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing 'project_path' argument"))?;
        let file_path = args["file_path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing 'file_path' argument"))?;
        let new_content = args["new_content"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing 'new_content' argument"))?;

        let root = PathBuf::from(project_path);
        let target = PathBuf::from(file_path);
        let ext = target
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        let language = Language::from_extension(ext);
        Ok((root, target, new_content.to_string(), language))
    }

    /// Run the path sandbox check. Returns the canonical target path if it
    /// lives inside the canonical project root. On escape, fires a CRITICAL
    /// toast and returns a `Block(message)` wrapped in `Ok` so the handler
    /// can return that text to the caller without surfacing an MCP error.
    fn sandbox_check(root: &Path, target: &Path) -> Result<SandboxResult> {
        match security::canonicalize_under_root(root, target) {
            Ok(canon) => Ok(SandboxResult::Ok(canon)),
            Err(e) => {
                notify::path_escape_blocked(&target.display().to_string());
                Ok(SandboxResult::Blocked(format!(
                    "PATH ESCAPE BLOCKED — CRITICAL\n\
                     ==============================\n\
                     {e}\n\
                     FILE NOT WRITTEN. This is a security check — AetherLink will never write \
                     a file outside the project root, even if a rule would allow it."
                )))
            }
        }
    }

    /// Validate a proposed file edit against project rules **without** writing
    /// it. The on-disk state of the project is never touched.
    ///
    /// `apply_guarded_change` is the writer counterpart — use that when you
    /// actually want the file saved.
    fn run_check_change(args: &Value) -> Result<String> {
        let (root, target, new_content, language) = Self::extract_change_args(args)?;

        if language == Language::Unknown {
            let ext = target.extension().and_then(|e| e.to_str()).unwrap_or("");
            return Ok(format!(
                "WRITE BLOCKED\n=============\nUnknown language for extension '{ext}'. AetherLink only supports rs, ts, tsx, js, jsx, py."
            ));
        }

        // Sandbox check applies even to dry-runs: there is no legitimate
        // reason for an agent to ask "would it be OK to write outside the
        // project?", and refusing here trains agents not to try.
        let canonical_target = match Self::sandbox_check(&root, &target)? {
            SandboxResult::Ok(p) => p,
            SandboxResult::Blocked(msg) => return Ok(msg),
        };
        let _ = canonical_target; // we use the original `target` for messaging below

        let pc = Self::evaluate_proposed_change(&root, target, &new_content, language)?;

        if pc.violations.is_empty() {
            // No status mutation here — `check_change` is a hypothetical, it
            // shouldn't overwrite the project's last real scan result.
            let warning_block = if pc.warnings.is_empty() {
                String::new()
            } else {
                let lines: Vec<String> = pc
                    .warnings
                    .iter()
                    .enumerate()
                    .map(|(i, w)| format!("  ({}) {}: {}", i + 1, w.rule, w.message))
                    .collect();
                format!(
                    "\n\n{} non-blocking warning{} (grandfathered or severity=warn):\n{}",
                    pc.warnings.len(),
                    if pc.warnings.len() == 1 { "" } else { "s" },
                    lines.join("\n")
                )
            };
            return Ok(format!(
                "APPROVED: proposed change to {} passes all architectural rules.\n\
                 - new line count: {}\n\
                 - parsed imports: {}\n\
                 - rules enforced: max_file_lines={:?}, no_cycles={}, forbidden_imports={}{}\n\
                 Safe to write — but use `apply_guarded_change` to actually save it; that tool re-validates and writes atomically so a stale `check_change` result can never be silently bypassed.",
                pc.target.display(),
                pc.synthetic.line_count,
                pc.synthetic.imports.len(),
                pc.rules.max_file_lines,
                pc.rules.no_cycles,
                pc.rules.forbidden_imports.len(),
                warning_block,
            ));
        }

        // Fire a toast so the user sees the block even if Claude's window is
        // not focused. Status file is left untouched — see APPROVED branch.
        notify::write_blocked(&pc.target.display().to_string(), pc.violations.len());

        Ok(Self::format_block_report(&pc.target, &pc.violations))
    }

    /// **The guarded writer.** Validate, then write *only if legal*.
    ///
    /// This is the atomic operation `check_change` was missing: there's no
    /// gap between "we said it was OK" and "we wrote it." A bypass would
    /// require Claude to call a separate file-write tool, which AetherLink
    /// loudly tells it not to do via the tool description.
    fn run_apply_guarded_change(args: &Value) -> Result<String> {
        let (root, target, new_content, language) = Self::extract_change_args(args)?;

        if language == Language::Unknown {
            let ext = target.extension().and_then(|e| e.to_str()).unwrap_or("");
            return Ok(format!(
                "WRITE BLOCKED — UNSUPPORTED LANGUAGE\n\
                 ====================================\n\
                 Unknown language for extension '{ext}'. AetherLink only supports rs, ts, tsx, js, jsx, py.\n\
                 FILE NOT WRITTEN."
            ));
        }

        // SECURITY GATE 1: refuse anything that escapes the project root.
        // This runs BEFORE the bypass check on purpose — even with bypass
        // engaged, we will not write outside the sandbox. The bypass switch
        // only overrides architectural rules, not security boundaries.
        let canonical_target = match Self::sandbox_check(&root, &target)? {
            SandboxResult::Ok(p) => p,
            SandboxResult::Blocked(msg) => return Ok(msg),
        };

        let pc = Self::evaluate_proposed_change(&root, target.clone(), &new_content, language)?;

        // SECURITY GATE 2: kill switch / break-glass override.
        // If `.aetherlink_bypass` exists in the project root, we write the
        // file regardless of validation result, but fire a CRITICAL warning
        // toast and mark the status update so the user can see in the tray
        // that the safety net is currently off.
        if security::bypass_engaged(&root) {
            return Self::write_with_bypass(&root, &canonical_target, &new_content, &pc);
        }

        if !pc.violations.is_empty() {
            // Gatekeeper: refuse to write. Surface the violations + a toast,
            // and DO NOT touch disk. The previously stored status (which
            // reflects the *current* on-disk project) is left as-is, since
            // nothing about the project actually changed.
            notify::write_blocked(&pc.target.display().to_string(), pc.violations.len());
            let body = Self::format_block_report(&pc.target, &pc.violations);
            return Ok(format!(
                "{body}\
                 ===\n\
                 FILE NOT WRITTEN. AetherLink refused this edit because it would break the rules above. \
                 Fix the violations and call apply_guarded_change again. The on-disk project is unchanged.\n"
            ));
        }

        // Validation passed. Make sure the parent dir exists (so the tool
        // works for brand-new files in fresh subdirectories), then write to
        // the canonicalized path so symlinks/`..` segments don't sneak in.
        if let Some(parent) = canonical_target.parent() {
            if !parent.as_os_str().is_empty() && !parent.exists() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("creating parent directory {}", parent.display()))?;
            }
        }
        fs::write(&canonical_target, &new_content)
            .with_context(|| format!("writing {}", canonical_target.display()))?;

        // The validation we just ran was over the *whole* project graph with
        // this file substituted in. Since we just made that substitution real
        // on disk, we can update the status file to LEGAL directly without
        // paying for another full scan. Mirror `run_scan`'s "back to green"
        // toast logic so the user gets feedback when a previously red project
        // is now clean.
        let was_illegal = matches!(
            status::read().ok().flatten().map(|s| s.state),
            Some(State::Illegal)
        );
        status::write(&Status::now(
            State::Legal,
            root.display().to_string(),
            Vec::new(),
            "apply_guarded_change",
        ));
        if was_illegal {
            notify::scan_legal(&root.display().to_string());
        }

        Ok(format!(
            "APPLIED: {} written to disk ({} bytes, {} lines, {} import{} parsed).\n\
             All architectural rules passed. Project is LEGAL.",
            pc.target.display(),
            new_content.len(),
            pc.synthetic.line_count,
            pc.synthetic.imports.len(),
            if pc.synthetic.imports.len() == 1 { "" } else { "s" },
        ))
    }

    /// Bypass-mode write path. Reached when `.aetherlink_bypass` exists in
    /// the project root. Writes the file regardless of validation result,
    /// fires a CRITICAL warning toast, and reflects the actual scan outcome
    /// in the status file (so the tray still turns red if the bypassed write
    /// was illegal — the toast and the red icon together signal "you wrote
    /// broken code on purpose, the safety net is off").
    fn write_with_bypass(
        root: &Path,
        canonical_target: &Path,
        new_content: &str,
        pc: &ProposedChange,
    ) -> Result<String> {
        // Same parent-creation step as the normal write path.
        if let Some(parent) = canonical_target.parent() {
            if !parent.as_os_str().is_empty() && !parent.exists() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("creating parent directory {}", parent.display()))?;
            }
        }
        fs::write(canonical_target, new_content)
            .with_context(|| format!("writing {}", canonical_target.display()))?;

        // Critical toast — fires on every bypassed write, regardless of
        // whether the content would have validated.
        notify::bypass_active(
            &canonical_target.display().to_string(),
            pc.violations.len(),
        );

        // Update status to reflect the actual scan result. If the bypassed
        // write was clean, status flips to Legal; if it was illegal, status
        // is Illegal so the tray turns red and the user knows the safety net
        // is off AND something is broken.
        let (state, summaries) = if pc.violations.is_empty() {
            (State::Legal, Vec::new())
        } else {
            (State::Illegal, Self::summarize(&pc.violations))
        };
        status::write(&Status::now(
            state,
            root.display().to_string(),
            summaries,
            "apply_guarded_change_BYPASS",
        ));

        let validation_summary = if pc.violations.is_empty() {
            "Validation would have PASSED — write was clean.".to_string()
        } else {
            format!(
                "Validation would have FAILED with {} violation{}:\n{}",
                pc.violations.len(),
                if pc.violations.len() == 1 { "" } else { "s" },
                pc.violations
                    .iter()
                    .enumerate()
                    .map(|(i, v)| format!("  [{}] {}: {}", i + 1, v.rule, v.message))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        };

        Ok(format!(
            "BYPASS ACTIVE — CRITICAL WARNING\n\
             ================================\n\
             AetherLink wrote {} WITHOUT enforcing architectural rules because \
             '.aetherlink_bypass' exists in the project root.\n\n\
             {}\n\n\
             Delete .aetherlink_bypass from the project root to re-enable normal validation.",
            canonical_target.display(),
            validation_summary
        ))
    }

    /// Convert validator output into the lightweight summaries the tray reads.
    fn summarize(violations: &[RuleViolation]) -> Vec<ViolationSummary> {
        violations
            .iter()
            .map(|v| ViolationSummary {
                rule: v.rule.clone(),
                message: v.message.clone(),
            })
            .collect()
    }

    fn format_block_report(target: &Path, violations: &[RuleViolation]) -> String {
        let mut report = String::new();
        report.push_str("WRITE BLOCKED\n");
        report.push_str("=============\n");
        report.push_str(&format!(
            "Proposed change to {} would introduce {} rule violation{}.\n",
            target.display(),
            violations.len(),
            if violations.len() == 1 { "" } else { "s" },
        ));
        report.push_str("Do NOT write this file. Fix the issues below and call check_change again.\n\n");
        for (i, v) in violations.iter().enumerate() {
            report.push_str(&format!(
                "[{}] {:?} — rule: {}\n    {}\n\n",
                i + 1,
                v.severity,
                v.rule,
                v.message
            ));
        }
        report
    }

    /// Synchronously scan a project, build the graph, and enforce architectural rules.
    ///
    /// If `AetherLink.toml` defines rules and any are violated, this returns a
    /// `CRITICAL ARCHITECTURAL VIOLATION` report instead of the normal summary.
    /// The build is "illegal" until every listed violation is resolved.
    fn run_scan(args: &Value) -> Result<String> {
        let path = args["path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing 'path' argument"))?;
        let root = PathBuf::from(path);

        let scanner = FileScanner::new(&root);
        let files = scanner.scan()?;

        let mut graph = DependencyGraph::new(&root);
        graph.build(&files);

        let rules_file = RulesFile::load(&root)?;
        let rules = rules_file.rules.clone();
        let raw = validate_with_overrides(&files, &graph, &rules_file, Some(&root));

        // Apply the same ratchet as the guarded-write path: split against the
        // committed baseline so pre-existing rot is shown as warnings rather
        // than treated as an architectural failure of the build.
        let committed_baseline = Baseline::load(&root)?;
        let (mut violations, mut warnings) =
            split_against_baseline(raw, committed_baseline.as_ref());

        // Demote `severity = "warn"` violations.
        let mut still_blocking = Vec::new();
        for v in violations.drain(..) {
            if v.severity == Severity::Warning {
                warnings.push(v);
            } else {
                still_blocking.push(v);
            }
        }
        let violations = still_blocking;

        if !violations.is_empty() {
            let mut report = String::new();
            report.push_str("CRITICAL ARCHITECTURAL VIOLATION\n");
            report.push_str("================================\n");
            report.push_str(&format!(
                "AetherLink found {} rule violation{} in {}.\n",
                violations.len(),
                if violations.len() == 1 { "" } else { "s" },
                root.display()
            ));
            report.push_str(
                "The build is ILLEGAL until every issue below is resolved.\n\n",
            );
            for (i, v) in violations.iter().enumerate() {
                report.push_str(&format!(
                    "[{}] {:?} — rule: {}\n    {}\n\n",
                    i + 1,
                    v.severity,
                    v.rule,
                    v.message
                ));
            }
            if !warnings.is_empty() {
                report.push_str(&format!(
                    "\nAlso {} warning{} (informational, not blocking):\n",
                    warnings.len(),
                    if warnings.len() == 1 { "" } else { "s" },
                ));
                for (i, v) in warnings.iter().enumerate() {
                    report.push_str(&format!(
                        "  ({}) {}: {}\n",
                        i + 1,
                        v.rule,
                        v.message
                    ));
                }
                report.push('\n');
            }
            report.push_str(
                "Fix every violation above, then re-run scan_project to verify the project is legal.\n",
            );

            // Publish the result to the tray and fire a toast.
            status::write(&Status::now(
                State::Illegal,
                root.display().to_string(),
                Self::summarize(&violations),
                "scan_project",
            ));
            notify::scan_illegal(&root.display().to_string(), violations.len());

            return Ok(report);
        }

        // Clean scan: publish LEGAL state and fire a "back to green" toast
        // only if the previous state on disk was illegal. Avoid spamming the
        // user with a toast on every routine scan.
        let was_illegal = matches!(
            status::read().ok().flatten().map(|s| s.state),
            Some(State::Illegal)
        );
        status::write(&Status::now(
            State::Legal,
            root.display().to_string(),
            Vec::new(),
            "scan_project",
        ));
        if was_illegal {
            notify::scan_legal(&root.display().to_string());
        }

        let summary = json!({
            "status": "LEGAL",
            "root": root.display().to_string(),
            "files_scanned": files.len(),
            "graph_nodes": graph.node_count(),
            "graph_edges": graph.edge_count(),
            "warnings_count": warnings.len(),
            "baseline_active": committed_baseline.is_some(),
            "rules_loaded": {
                "max_file_lines": rules.max_file_lines,
                "no_cycles": rules.no_cycles,
                "forbidden_imports": rules.forbidden_imports.len(),
                "default_severity": format!("{:?}", rules.default_severity).to_lowercase(),
            },
            "files": files.iter().map(|f| json!({
                "path": f.path.display().to_string(),
                "language": format!("{:?}", f.language),
                "line_count": f.line_count,
                "import_count": f.imports.len(),
                "imports": f.imports,
            })).collect::<Vec<_>>(),
        });
        Ok(serde_json::to_string_pretty(&summary)?)
    }

    fn error_response(&self, id: &Value, code: i32, message: &str) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": code,
                "message": message
            }
        })
    }
}

/// Translate the loose `(rule_type, params)` shape coming over MCP into a typed
/// `RuleAddition`. Accepts several common phrasings so an AI client doesn't
/// have to remember the exact key names.
fn parse_rule_addition(rule_type: &str, params: &Value) -> Result<RuleAddition> {
    match rule_type {
        "max_file_lines" | "line_limit" => {
            // Accept {value: N}, {max: N}, {limit: N}, or a bare integer.
            let n = if let Some(n) = params.as_i64() {
                n
            } else {
                params["value"]
                    .as_i64()
                    .or_else(|| params["max"].as_i64())
                    .or_else(|| params["limit"].as_i64())
                    .ok_or_else(|| anyhow::anyhow!(
                        "{rule_type} expects params like {{\"value\": 500}}"
                    ))?
            };
            Ok(RuleAddition::MaxFileLines(n))
        }
        "no_cycles" => {
            let b = if let Some(b) = params.as_bool() {
                b
            } else {
                params["enabled"]
                    .as_bool()
                    .or_else(|| params["value"].as_bool())
                    .ok_or_else(|| anyhow::anyhow!(
                        "no_cycles expects params like {{\"enabled\": true}}"
                    ))?
            };
            Ok(RuleAddition::NoCycles(b))
        }
        "forbidden_import" | "forbidden_imports" => {
            let (from, to) = parse_forbidden_params(params)?;
            Ok(RuleAddition::ForbiddenImport { from, to })
        }
        other => Err(anyhow::anyhow!(
            "unknown rule_type '{other}'. Use one of: forbidden_import, line_limit (max_file_lines), no_cycles."
        )),
    }
}

/// Translate a `remove_rule` MCP call into a typed `RuleRemoval`. Mirrors
/// `parse_rule_addition` so the same loose JSON shapes work for both.
fn parse_rule_removal(rule_type: &str, params: &Value) -> Result<RuleRemoval> {
    match rule_type {
        "max_file_lines" | "line_limit" => Ok(RuleRemoval::MaxFileLines),
        "no_cycles" => Ok(RuleRemoval::NoCycles),
        "forbidden_import" | "forbidden_imports" => {
            let (from, to) = parse_forbidden_params(params)?;
            Ok(RuleRemoval::ForbiddenImport { from, to })
        }
        other => Err(anyhow::anyhow!(
            "unknown rule_type '{other}'. Use one of: forbidden_import, line_limit (max_file_lines), no_cycles."
        )),
    }
}

/// Pull a `(from, to)` pair out of the loose `params` shape we accept for
/// forbidden imports. Used by both add and remove parsers, so the supported
/// shapes stay identical between the two tools.
fn parse_forbidden_params(params: &Value) -> Result<(String, String)> {
    if let Some(s) = params.as_str() {
        return parse_arrow_pair(s);
    }
    if let Some(s) = params["rule"].as_str() {
        return parse_arrow_pair(s);
    }
    let from = params["from"].as_str().ok_or_else(|| {
        anyhow::anyhow!(
            "forbidden_import expects params like {{\"from\": \"ui\", \"to\": \"db\"}}"
        )
    })?;
    let to = params["to"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("forbidden_import params missing 'to'"))?;
    Ok((from.to_string(), to.to_string()))
}

fn parse_arrow_pair(s: &str) -> Result<(String, String)> {
    let (from, to) = s
        .split_once("->")
        .ok_or_else(|| anyhow::anyhow!("expected 'from -> to', got '{s}'"))?;
    let from = from.trim().to_string();
    let to = to.trim().to_string();
    if from.is_empty() || to.is_empty() {
        return Err(anyhow::anyhow!("forbidden_import has empty side: '{s}'"));
    }
    Ok((from, to))
}

/// Render a `Duration` as `Nm Ms` (e.g. `4m 32s`) for human-friendly messages.
fn format_duration(d: Duration) -> String {
    let secs = d.as_secs();
    let m = secs / 60;
    let s = secs % 60;
    if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    }
}
