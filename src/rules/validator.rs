use std::path::{Path, PathBuf};

use crate::graph::DependencyGraph;
use crate::scanner::ScannedFile;

use super::{ForbiddenImport, RuleViolation, Rules, Severity};

/// Run every enabled rule against the project and collect violations.
///
/// `files` is the scan output (used for per-file checks like line counts);
/// `graph` is the resolved dependency graph (used for edge & cycle checks).
pub fn validate(
    files: &[ScannedFile],
    graph: &DependencyGraph,
    rules: &Rules,
) -> Vec<RuleViolation> {
    let mut violations = Vec::new();

    if let Some(limit) = rules.max_file_lines {
        check_max_file_lines(files, limit, &mut violations);
    }

    if !rules.forbidden_imports.is_empty() {
        check_forbidden_imports(graph, &rules.forbidden_imports, &mut violations);
    }

    if rules.no_cycles {
        check_no_cycles(graph, &mut violations);
    }

    violations
}

// ---------- max_file_lines ----------

fn check_max_file_lines(files: &[ScannedFile], limit: usize, out: &mut Vec<RuleViolation>) {
    for file in files {
        if file.line_count > limit {
            out.push(RuleViolation {
                rule: "max_file_lines".into(),
                severity: Severity::Error,
                message: format!(
                    "{} has {} lines, exceeds limit of {}",
                    file.path.display(),
                    file.line_count,
                    limit
                ),
            });
        }
    }
}

// ---------- forbidden_imports ----------

fn check_forbidden_imports(
    graph: &DependencyGraph,
    rules: &[ForbiddenImport],
    out: &mut Vec<RuleViolation>,
) {
    for (from_path, to_path) in graph.edges() {
        for rule in rules {
            if path_in_folder(from_path, &rule.from) && path_in_folder(to_path, &rule.to) {
                out.push(RuleViolation {
                    rule: "forbidden_imports".into(),
                    severity: Severity::Error,
                    message: format!(
                        "{} (in '{}') imports {} (in '{}'), which is forbidden by rule '{} -> {}'",
                        from_path.display(),
                        rule.from,
                        to_path.display(),
                        rule.to,
                        rule.from,
                        rule.to
                    ),
                });
            }
        }
    }
}

/// True if any directory component of `path` equals `folder`.
/// e.g. `src/ui/button.tsx` is "in" folder `ui`.
fn path_in_folder(path: &Path, folder: &str) -> bool {
    let Some(parent) = path.parent() else { return false };
    parent
        .components()
        .any(|c| c.as_os_str().to_str() == Some(folder))
}

// ---------- no_cycles ----------

fn check_no_cycles(graph: &DependencyGraph, out: &mut Vec<RuleViolation>) {
    for cycle in graph.find_cycles() {
        let files: Vec<String> = cycle.iter().map(|p| p.display().to_string()).collect();
        let representative: PathBuf = cycle[0].clone();
        out.push(RuleViolation {
            rule: "no_cycles".into(),
            severity: Severity::Error,
            message: format!(
                "Circular dependency detected among {} files (starting at {}): {}",
                cycle.len(),
                representative.display(),
                files.join(" -> ")
            ),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::Language;

    fn dummy_file(path: &str, lines: usize) -> ScannedFile {
        ScannedFile {
            path: PathBuf::from(path),
            language: Language::Rust,
            imports: Vec::new(),
            exports: Vec::new(),
            line_count: lines,
        }
    }

    #[test]
    fn max_file_lines_flags_oversized_files() {
        let files = vec![
            dummy_file("src/small.rs", 100),
            dummy_file("src/big.rs", 600),
            dummy_file("src/exact.rs", 500),
        ];
        let graph = DependencyGraph::new("/project");
        let rules = Rules {
            max_file_lines: Some(500),
            ..Default::default()
        };
        let v = validate(&files, &graph, &rules);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].rule, "max_file_lines");
        assert!(v[0].message.contains("big.rs"));
        assert!(v[0].message.contains("600"));
    }

    #[test]
    fn max_file_lines_disabled_when_none() {
        let files = vec![dummy_file("src/huge.rs", 99_999)];
        let graph = DependencyGraph::new("/project");
        let rules = Rules::default();
        assert!(validate(&files, &graph, &rules).is_empty());
    }

    #[test]
    fn forbidden_imports_blocks_matching_edge() {
        let mut graph = DependencyGraph::new("/project");
        graph.add_file("src/ui/button.rs");
        graph.add_file("src/db/conn.rs");
        graph.add_file("src/api/handler.rs");
        graph.add_dependency(Path::new("src/ui/button.rs"), Path::new("src/db/conn.rs"));
        graph.add_dependency(Path::new("src/api/handler.rs"), Path::new("src/db/conn.rs"));

        let rules = Rules {
            forbidden_imports: vec![ForbiddenImport {
                from: "ui".into(),
                to: "db".into(),
            }],
            ..Default::default()
        };

        let v = validate(&[], &graph, &rules);
        assert_eq!(v.len(), 1, "only ui->db should be flagged, not api->db");
        assert_eq!(v[0].rule, "forbidden_imports");
        assert!(v[0].message.contains("button.rs"));
        assert!(v[0].message.contains("conn.rs"));
    }

    #[test]
    fn forbidden_imports_allows_unrelated_edges() {
        let mut graph = DependencyGraph::new("/project");
        graph.add_file("src/ui/a.rs");
        graph.add_file("src/ui/b.rs");
        graph.add_dependency(Path::new("src/ui/a.rs"), Path::new("src/ui/b.rs"));

        let rules = Rules {
            forbidden_imports: vec![ForbiddenImport {
                from: "ui".into(),
                to: "db".into(),
            }],
            ..Default::default()
        };
        assert!(validate(&[], &graph, &rules).is_empty());
    }

    #[test]
    fn no_cycles_detects_simple_cycle() {
        let mut graph = DependencyGraph::new("/project");
        graph.add_file("a.rs");
        graph.add_file("b.rs");
        graph.add_file("c.rs");
        graph.add_dependency(Path::new("a.rs"), Path::new("b.rs"));
        graph.add_dependency(Path::new("b.rs"), Path::new("c.rs"));
        graph.add_dependency(Path::new("c.rs"), Path::new("a.rs"));

        let rules = Rules {
            no_cycles: true,
            ..Default::default()
        };
        let v = validate(&[], &graph, &rules);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].rule, "no_cycles");
        assert!(v[0].message.contains("3 files"));
    }

    #[test]
    fn no_cycles_passes_on_dag() {
        let mut graph = DependencyGraph::new("/project");
        graph.add_file("a.rs");
        graph.add_file("b.rs");
        graph.add_file("c.rs");
        graph.add_dependency(Path::new("a.rs"), Path::new("b.rs"));
        graph.add_dependency(Path::new("b.rs"), Path::new("c.rs"));

        let rules = Rules {
            no_cycles: true,
            ..Default::default()
        };
        assert!(validate(&[], &graph, &rules).is_empty());
    }

    #[test]
    fn no_cycles_disabled_when_false() {
        let mut graph = DependencyGraph::new("/project");
        graph.add_file("a.rs");
        graph.add_dependency(Path::new("a.rs"), Path::new("a.rs"));
        let rules = Rules::default();
        assert!(validate(&[], &graph, &rules).is_empty());
    }

    #[test]
    fn all_rules_combined() {
        let mut graph = DependencyGraph::new("/project");
        graph.add_file("src/ui/x.rs");
        graph.add_file("src/db/y.rs");
        graph.add_dependency(Path::new("src/ui/x.rs"), Path::new("src/db/y.rs"));
        graph.add_dependency(Path::new("src/db/y.rs"), Path::new("src/ui/x.rs"));

        let files = vec![dummy_file("src/ui/x.rs", 1000)];
        let rules = Rules {
            max_file_lines: Some(500),
            no_cycles: true,
            forbidden_imports: vec![ForbiddenImport {
                from: "ui".into(),
                to: "db".into(),
            }],
        };
        let v = validate(&files, &graph, &rules);
        // 1 oversized file + 1 forbidden edge + 1 cycle
        assert_eq!(v.len(), 3);
        let kinds: Vec<&str> = v.iter().map(|r| r.rule.as_str()).collect();
        assert!(kinds.contains(&"max_file_lines"));
        assert!(kinds.contains(&"forbidden_imports"));
        assert!(kinds.contains(&"no_cycles"));
    }
}
