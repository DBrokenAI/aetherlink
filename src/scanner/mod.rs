mod file_scanner;
mod import_parser;

pub use file_scanner::FileScanner;
pub use import_parser::ImportParser;

use std::path::PathBuf;

/// Metadata about a scanned file.
#[derive(Debug, Clone)]
pub struct ScannedFile {
    pub path: PathBuf,
    pub language: Language,
    pub imports: Vec<String>,
    pub exports: Vec<String>,
    /// Number of lines in the file (counted at scan time, not re-read).
    pub line_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Language {
    Rust,
    TypeScript,
    JavaScript,
    Python,
    Unknown,
}

impl Language {
    pub fn from_extension(ext: &str) -> Self {
        match ext {
            "rs" => Language::Rust,
            "ts" | "tsx" => Language::TypeScript,
            "js" | "jsx" => Language::JavaScript,
            "py" => Language::Python,
            _ => Language::Unknown,
        }
    }
}
