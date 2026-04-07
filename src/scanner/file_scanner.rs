use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use walkdir::WalkDir;

use super::{ImportParser, Language, ScannedFile};

/// Walks a project directory and collects file metadata + imports.
pub struct FileScanner {
    root: PathBuf,
    ignore_patterns: Vec<String>,
}

impl FileScanner {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            ignore_patterns: vec![
                "target".into(),
                "node_modules".into(),
                ".git".into(),
                "__pycache__".into(),
                "dist".into(),
                "build".into(),
                ".venv".into(),
                "venv".into(),
            ],
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Walk the project directory, parse imports, return one ScannedFile per source file.
    pub fn scan(&self) -> Result<Vec<ScannedFile>> {
        let mut parser = ImportParser::new()?;
        let mut files = Vec::new();

        for entry in WalkDir::new(&self.root)
            .into_iter()
            .filter_entry(|e| !self.is_ignored(e.path()))
        {
            let entry = match entry {
                Ok(e) => e,
                Err(err) => {
                    tracing::warn!("Skipping entry due to error: {err}");
                    continue;
                }
            };

            if !entry.file_type().is_file() {
                continue;
            }

            let path = entry.path();
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            let language = Language::from_extension(ext);

            if language == Language::Unknown {
                continue;
            }

            // Read the file. Skip on read failure (e.g., permissions, non-UTF8).
            let source = match fs::read_to_string(path) {
                Ok(s) => s,
                Err(err) => {
                    tracing::warn!("Failed to read {}: {err}", path.display());
                    continue;
                }
            };

            let imports = parser.parse(&source, &language);
            let line_count = source.lines().count();

            files.push(ScannedFile {
                path: path.to_path_buf(),
                language,
                imports,
                exports: Vec::new(),
                line_count,
            });
        }

        tracing::info!("Scanned {} source files", files.len());
        Ok(files)
    }

    fn is_ignored(&self, path: &Path) -> bool {
        path.file_name()
            .and_then(|n| n.to_str())
            .map(|name| self.ignore_patterns.iter().any(|p| name == p))
            .unwrap_or(false)
    }
}
