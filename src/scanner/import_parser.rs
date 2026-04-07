use anyhow::Result;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, Query, QueryCursor};

use super::Language;

/// Extracts import/dependency strings from source code using tree-sitter.
pub struct ImportParser {
    rust_parser: Parser,
    rust_query: Query,
    python_parser: Parser,
    python_query: Query,
    javascript_parser: Parser,
    javascript_query: Query,
    typescript_parser: Parser,
    typescript_query: Query,
}

impl ImportParser {
    pub fn new() -> Result<Self> {
        let rust_lang = tree_sitter_rust::LANGUAGE;
        let python_lang = tree_sitter_python::LANGUAGE;
        let js_lang = tree_sitter_javascript::LANGUAGE;
        let ts_lang = tree_sitter_typescript::LANGUAGE_TYPESCRIPT;

        let mut rust_parser = Parser::new();
        rust_parser.set_language(&rust_lang.into())?;

        let mut python_parser = Parser::new();
        python_parser.set_language(&python_lang.into())?;

        let mut javascript_parser = Parser::new();
        javascript_parser.set_language(&js_lang.into())?;

        let mut typescript_parser = Parser::new();
        typescript_parser.set_language(&ts_lang.into())?;

        // Rust: capture use declarations and mod declarations
        let rust_query = Query::new(
            &rust_lang.into(),
            r#"
            (use_declaration argument: (_) @import)
            (mod_item name: (identifier) @mod_name
              !body)
            "#,
        )?;

        // Python: import X / from X import Y
        let python_query = Query::new(
            &python_lang.into(),
            r#"
            (import_statement name: (dotted_name) @import)
            (import_from_statement module_name: (dotted_name) @import)
            (import_from_statement module_name: (relative_import) @import)
            "#,
        )?;

        // JavaScript: import ... from 'X' / require('X')
        let javascript_query = Query::new(
            &js_lang.into(),
            r#"
            (import_statement source: (string) @import)
            (call_expression
              function: (identifier) @_func
              arguments: (arguments (string) @import)
              (#eq? @_func "require"))
            "#,
        )?;

        // TypeScript: same as JS
        let typescript_query = Query::new(
            &ts_lang.into(),
            r#"
            (import_statement source: (string) @import)
            (call_expression
              function: (identifier) @_func
              arguments: (arguments (string) @import)
              (#eq? @_func "require"))
            "#,
        )?;

        Ok(Self {
            rust_parser,
            rust_query,
            python_parser,
            python_query,
            javascript_parser,
            javascript_query,
            typescript_parser,
            typescript_query,
        })
    }

    /// Parse imports from source code for the given language.
    pub fn parse(&mut self, source: &str, language: &Language) -> Vec<String> {
        match language {
            Language::Rust => self.parse_with(&mut Self::rust_pair, source),
            Language::Python => self.parse_with(&mut Self::python_pair, source),
            Language::JavaScript => self.parse_with(&mut Self::js_pair, source),
            Language::TypeScript => self.parse_with(&mut Self::ts_pair, source),
            Language::Unknown => Vec::new(),
        }
    }

    // Helper that borrows the right parser+query pair and runs extraction.
    // We use separate methods to avoid borrow-checker issues with &mut self.

    fn rust_pair(&mut self) -> (&mut Parser, &Query) {
        (&mut self.rust_parser, &self.rust_query)
    }

    fn python_pair(&mut self) -> (&mut Parser, &Query) {
        (&mut self.python_parser, &self.python_query)
    }

    fn js_pair(&mut self) -> (&mut Parser, &Query) {
        (&mut self.javascript_parser, &self.javascript_query)
    }

    fn ts_pair(&mut self) -> (&mut Parser, &Query) {
        (&mut self.typescript_parser, &self.typescript_query)
    }

    fn parse_with(
        &mut self,
        get_pair: &mut dyn FnMut(&mut Self) -> (&mut Parser, &Query),
        source: &str,
    ) -> Vec<String> {
        let (parser, query) = get_pair(self);
        let Some(tree) = parser.parse(source, None) else {
            return Vec::new();
        };

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(query, tree.root_node(), source.as_bytes());

        let mut imports = Vec::new();
        while let Some(m) = matches.next() {
            for capture in m.captures {
                // Skip helper captures whose names start with `_` (e.g. @_func predicates).
                let name = &query.capture_names()[capture.index as usize];
                if name.starts_with('_') {
                    continue;
                }
                let text = &source[capture.node.byte_range()];
                // Strip quotes from JS/TS string literals
                let cleaned = text.trim_matches(|c| c == '"' || c == '\'');
                if !cleaned.is_empty() {
                    imports.push(cleaned.to_string());
                }
            }
        }

        imports
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rust_imports() {
        let mut parser = ImportParser::new().unwrap();
        let source = r#"
use std::collections::HashMap;
use crate::scanner::FileScanner;
mod graph;
"#;
        let imports = parser.parse(source, &Language::Rust);
        assert!(imports.contains(&"std::collections::HashMap".to_string()));
        assert!(imports.contains(&"crate::scanner::FileScanner".to_string()));
        assert!(imports.contains(&"graph".to_string()));
    }

    #[test]
    fn test_python_imports() {
        let mut parser = ImportParser::new().unwrap();
        let source = r#"
import os
from pathlib import Path
from . import utils
"#;
        let imports = parser.parse(source, &Language::Python);
        assert!(imports.contains(&"os".to_string()));
        assert!(imports.contains(&"pathlib".to_string()));
    }

    #[test]
    fn test_javascript_imports() {
        let mut parser = ImportParser::new().unwrap();
        let source = r#"
import React from 'react';
import { useState } from 'react';
const fs = require('fs');
"#;
        let imports = parser.parse(source, &Language::JavaScript);
        assert!(imports.contains(&"react".to_string()));
        assert!(imports.contains(&"fs".to_string()));
    }

    #[test]
    fn test_typescript_imports() {
        let mut parser = ImportParser::new().unwrap();
        let source = r#"
import { Component } from '@angular/core';
import * as path from 'path';
"#;
        let imports = parser.parse(source, &Language::TypeScript);
        assert!(imports.contains(&"@angular/core".to_string()));
        assert!(imports.contains(&"path".to_string()));
    }
}
