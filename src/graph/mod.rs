use std::collections::HashMap;
use std::path::{Path, PathBuf};

use petgraph::algo::tarjan_scc;
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::EdgeRef;

use crate::scanner::{Language, ScannedFile};

/// Directed dependency graph of project files.
///
/// An edge `A -> B` means "A imports / depends on B".
pub struct DependencyGraph {
    graph: DiGraph<PathBuf, ()>,
    index_map: HashMap<PathBuf, NodeIndex>,
    /// Project root used for resolving relative/absolute imports.
    root: PathBuf,
}

impl DependencyGraph {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            graph: DiGraph::new(),
            index_map: HashMap::new(),
            root: root.into(),
        }
    }

    /// Build the graph from scanned files.
    pub fn build(&mut self, files: &[ScannedFile]) {
        // Register all nodes first.
        for file in files {
            let idx = self.graph.add_node(file.path.clone());
            self.index_map.insert(file.path.clone(), idx);
        }

        // Resolve imports to file paths and add edges.
        let mut unresolved = 0usize;
        for file in files {
            let from = self.index_map[&file.path];
            for import in &file.imports {
                if let Some(target) = self.resolve_import(&file.path, import, &file.language) {
                    if let Some(&to) = self.index_map.get(&target) {
                        self.graph.add_edge(from, to, ());
                        continue;
                    }
                }
                unresolved += 1;
            }
        }

        tracing::info!(
            "Built dependency graph: {} nodes, {} edges ({} imports unresolved/external)",
            self.graph.node_count(),
            self.graph.edge_count(),
            unresolved
        );
    }

    pub fn node_count(&self) -> usize {
        self.graph.node_count()
    }

    pub fn edge_count(&self) -> usize {
        self.graph.edge_count()
    }

    /// Get direct dependents of a file (who imports it).
    pub fn dependents_of(&self, path: &Path) -> Vec<&PathBuf> {
        let Some(&idx) = self.index_map.get(path) else {
            return Vec::new();
        };

        self.graph
            .neighbors_directed(idx, petgraph::Direction::Incoming)
            .map(|n| &self.graph[n])
            .collect()
    }

    /// Get direct dependencies of a file (what it imports).
    pub fn dependencies_of(&self, path: &Path) -> Vec<&PathBuf> {
        let Some(&idx) = self.index_map.get(path) else {
            return Vec::new();
        };

        self.graph
            .neighbors_directed(idx, petgraph::Direction::Outgoing)
            .map(|n| &self.graph[n])
            .collect()
    }

    /// All files in the graph, in insertion order.
    pub fn files(&self) -> impl Iterator<Item = &PathBuf> {
        self.graph.node_indices().map(|i| &self.graph[i])
    }

    /// Iterate over every edge as `(importer, imported)` path pairs.
    pub fn edges(&self) -> impl Iterator<Item = (&PathBuf, &PathBuf)> {
        self.graph
            .edge_references()
            .map(|e| (&self.graph[e.source()], &self.graph[e.target()]))
    }

    /// Find all cycles in the graph.
    ///
    /// Returns each strongly-connected component of size > 1, plus any node
    /// with a self-loop. Each inner Vec is a set of files that mutually depend
    /// on each other (directly or transitively).
    pub fn find_cycles(&self) -> Vec<Vec<&PathBuf>> {
        let mut cycles = Vec::new();
        for scc in tarjan_scc(&self.graph) {
            let is_cycle = scc.len() > 1
                || (scc.len() == 1 && self.graph.contains_edge(scc[0], scc[0]));
            if is_cycle {
                cycles.push(scc.into_iter().map(|n| &self.graph[n]).collect());
            }
        }
        cycles
    }

    /// Manually add a file node. Used by tests and callers that build a graph
    /// without going through `build()`.
    pub fn add_file(&mut self, path: impl Into<PathBuf>) {
        let path = path.into();
        if self.index_map.contains_key(&path) {
            return;
        }
        let idx = self.graph.add_node(path.clone());
        self.index_map.insert(path, idx);
    }

    /// Manually add a dependency edge between two already-added files.
    /// Returns false if either file is unknown.
    pub fn add_dependency(&mut self, from: &Path, to: &Path) -> bool {
        let (Some(&f), Some(&t)) = (self.index_map.get(from), self.index_map.get(to)) else {
            return false;
        };
        self.graph.add_edge(f, t, ());
        true
    }

    /// Resolve an import string to a file path in the project, if possible.
    /// Returns `None` for external/stdlib imports.
    fn resolve_import(
        &self,
        from: &Path,
        import: &str,
        language: &Language,
    ) -> Option<PathBuf> {
        match language {
            Language::Rust => self.resolve_rust(from, import),
            Language::Python => self.resolve_python(from, import),
            Language::JavaScript | Language::TypeScript => self.resolve_js(from, import),
            Language::Unknown => None,
        }
    }

    fn resolve_rust(&self, from: &Path, import: &str) -> Option<PathBuf> {
        // Strip trailing path segments — we only want to map to a *file*, not an item.
        // `crate::scanner::FileScanner` -> try `src/scanner/mod.rs`, `src/scanner.rs`, `src/scanner/file_scanner.rs`
        let segments: Vec<&str> = import.split("::").collect();

        // External crate? skip stdlib & unknown crates
        if segments.first().map(|s| *s == "std" || *s == "core" || *s == "alloc").unwrap_or(false) {
            return None;
        }

        // Walk segments greedily: try the longest path first, then drop the last segment.
        let base = if segments.first().copied() == Some("crate") {
            self.root.join("src")
        } else if segments.first().copied() == Some("super") {
            from.parent()?.parent()?.to_path_buf()
        } else if segments.first().copied() == Some("self") {
            from.parent()?.to_path_buf()
        } else {
            // Could be a sibling module — also try relative to current file's dir.
            self.root.join("src")
        };

        let path_segments: Vec<&str> = segments
            .iter()
            .copied()
            .filter(|s| *s != "crate" && *s != "self" && *s != "super")
            .collect();

        // Try progressively shorter prefixes (last segments may be types/functions).
        for end in (1..=path_segments.len()).rev() {
            let mut candidate_dir = base.clone();
            for s in &path_segments[..end - 1] {
                candidate_dir.push(s);
            }
            let last = path_segments[end - 1];

            let as_file = candidate_dir.join(format!("{last}.rs"));
            if as_file.exists() {
                return Some(as_file);
            }
            let as_mod = candidate_dir.join(last).join("mod.rs");
            if as_mod.exists() {
                return Some(as_mod);
            }
        }

        // Bare `mod foo;` style — sibling file
        if segments.len() == 1 {
            let parent = from.parent()?;
            let candidate = parent.join(format!("{}.rs", segments[0]));
            if candidate.exists() {
                return Some(candidate);
            }
        }

        None
    }

    fn resolve_python(&self, from: &Path, import: &str) -> Option<PathBuf> {
        // Relative imports start with `.`
        let (base, rest) = if let Some(stripped) = import.strip_prefix('.') {
            // Each leading dot = one level up
            let mut dots = 1;
            let mut rest = stripped;
            while let Some(s) = rest.strip_prefix('.') {
                dots += 1;
                rest = s;
            }
            let mut base = from.parent()?.to_path_buf();
            for _ in 1..dots {
                base = base.parent()?.to_path_buf();
            }
            (base, rest.to_string())
        } else {
            (self.root.clone(), import.to_string())
        };

        let parts: Vec<&str> = rest.split('.').filter(|s| !s.is_empty()).collect();
        if parts.is_empty() {
            // `from . import x` — the file itself / package init
            let init = base.join("__init__.py");
            return init.exists().then_some(init);
        }

        let mut candidate = base;
        for p in &parts {
            candidate.push(p);
        }

        let as_file = candidate.with_extension("py");
        if as_file.exists() {
            return Some(as_file);
        }
        let as_pkg = candidate.join("__init__.py");
        if as_pkg.exists() {
            return Some(as_pkg);
        }
        None
    }

    fn resolve_js(&self, from: &Path, import: &str) -> Option<PathBuf> {
        // Only resolve relative imports — bare specifiers are npm packages.
        if !import.starts_with('.') && !import.starts_with('/') {
            return None;
        }

        let parent = from.parent()?;
        let base = parent.join(import);

        let extensions = ["ts", "tsx", "js", "jsx", "mjs", "cjs"];

        // Try `import` as exact file. We must normalize before returning,
        // otherwise candidates like `src/screens/../data/recipes.js` exist on
        // disk but don't match the canonical keys in `index_map`, so the edge
        // is silently dropped and forbidden-import rules never fire.
        for ext in &extensions {
            let candidate = normalize_path(&base.with_extension(ext));
            if candidate.exists() {
                return Some(candidate);
            }
        }
        // Try as directory with index.*
        for ext in &extensions {
            let candidate = normalize_path(&base.join(format!("index.{ext}")));
            if candidate.exists() {
                return Some(candidate);
            }
        }
        // Already-extensioned import
        let base_norm = normalize_path(&base);
        if base_norm.exists() && base_norm.is_file() {
            return Some(base_norm);
        }
        None
    }
}

/// Collapse `.` and `..` components from a path lexically (no disk access).
/// Needed because the JS resolver builds candidates by joining a relative
/// import onto the importing file's parent, which leaves `..` segments in
/// place. `Path::exists()` happily follows them, but `HashMap` lookups
/// against canonical keys fail — so resolved edges go missing.
fn normalize_path(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}
