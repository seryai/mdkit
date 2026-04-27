//! Jupyter notebook (`.ipynb`) extraction.
//!
//! `.ipynb` files are JSON documents conforming to the
//! [Jupyter notebook format spec](https://nbformat.readthedocs.io/en/latest/format_description.html).
//! Each notebook contains an ordered list of *cells*, where every
//! cell carries a `cell_type` (`markdown`, `code`, or `raw`) and a
//! `source` that is either a string or a list of strings (one per
//! line, JSON-encoded that way for diff-friendliness).
//!
//! `IpynbExtractor` walks the cells in order and emits:
//!
//! - `markdown` cells → inline, verbatim
//! - `code` cells → wrapped in a fenced code block, language hint
//!   derived from the notebook's `metadata.kernelspec.language` (or
//!   `metadata.language_info.name`) when present
//! - `raw` cells → inline, verbatim (treat as opaque text per spec
//!   §"Raw `NBConvert` cells")
//!
//! Cell *outputs* are intentionally NOT included in the markdown
//! body — they're typically large (image data URLs, repr blobs)
//! and not what callers indexing notebooks for search / RAG want.
//! A future "rich extraction" trait could expose them.

use crate::{Document, Error, Extractor, Result};
use serde_json::Value;
use std::path::Path;

/// Jupyter-notebook extractor. Construct via [`IpynbExtractor::new`]
/// (cannot fail — pure-Rust JSON parse, no runtime dependency).
#[derive(Default)]
pub struct IpynbExtractor;

impl IpynbExtractor {
    /// Construct an extractor. Cannot fail.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Extractor for IpynbExtractor {
    fn extensions(&self) -> &[&'static str] {
        &["ipynb"]
    }

    fn name(&self) -> &'static str {
        "ipynb-builtin"
    }

    fn extract(&self, path: &Path) -> Result<Document> {
        let bytes = std::fs::read(path)?;
        self.extract_bytes(&bytes, "ipynb")
    }

    fn extract_bytes(&self, bytes: &[u8], _ext: &str) -> Result<Document> {
        let v: Value = serde_json::from_slice(bytes)
            .map_err(|e| Error::ParseError(format!("notebook is not valid JSON: {e}")))?;
        Ok(notebook_to_document(&v))
    }
}

/// Convert a parsed notebook JSON value into a [`Document`]. Pulled
/// out of the trait impl so we can unit-test against in-memory
/// JSON fixtures without touching the filesystem.
fn notebook_to_document(notebook: &Value) -> Document {
    // Language hint for code cells. Try kernelspec first (preferred),
    // fall back to language_info. Default to no hint — markdown's
    // un-hinted ``` is still valid.
    let language = notebook
        .pointer("/metadata/kernelspec/language")
        .and_then(Value::as_str)
        .or_else(|| {
            notebook
                .pointer("/metadata/language_info/name")
                .and_then(Value::as_str)
        })
        .unwrap_or("")
        .to_string();

    let mut markdown = String::new();
    if let Some(cells) = notebook.get("cells").and_then(Value::as_array) {
        for cell in cells {
            let cell_type = cell.get("cell_type").and_then(Value::as_str).unwrap_or("");
            let source = cell_source(cell);
            let trimmed = source.trim();
            if trimmed.is_empty() {
                continue;
            }
            if !markdown.is_empty() {
                markdown.push_str("\n\n");
            }
            match cell_type {
                "markdown" | "raw" => markdown.push_str(trimmed),
                "code" => {
                    markdown.push_str("```");
                    if !language.is_empty() {
                        markdown.push_str(&language);
                    }
                    markdown.push('\n');
                    markdown.push_str(trimmed);
                    markdown.push_str("\n```");
                }
                _ => {
                    // Unknown cell_type — emit as opaque text rather
                    // than dropping. Future Jupyter spec versions may
                    // introduce new types and we'd rather over-include
                    // than silently lose content.
                    markdown.push_str(trimmed);
                }
            }
        }
    }

    let title = notebook
        .pointer("/metadata/title")
        .and_then(Value::as_str)
        .map(str::to_string);

    let mut metadata = std::collections::HashMap::new();
    if !language.is_empty() {
        metadata.insert("kernel_language".into(), language.clone());
    }
    if let Some(kernel_name) = notebook
        .pointer("/metadata/kernelspec/display_name")
        .and_then(Value::as_str)
    {
        metadata.insert("kernel_display_name".into(), kernel_name.to_string());
    }

    Document {
        markdown,
        title,
        metadata,
    }
}

/// Extract the `source` field of a cell. Per the nbformat spec, it
/// can be either a string OR an array of strings (one per line, joined
/// without separators since each element keeps its trailing `\n`).
fn cell_source(cell: &Value) -> String {
    match cell.get("source") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(lines)) => {
            let mut out = String::new();
            for line in lines {
                if let Some(s) = line.as_str() {
                    out.push_str(s);
                }
            }
            out
        }
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extensions_is_ipynb_only() {
        assert_eq!(IpynbExtractor.extensions(), &["ipynb"]);
    }

    #[test]
    fn name_identifies_backend() {
        assert_eq!(IpynbExtractor.name(), "ipynb-builtin");
    }

    #[test]
    fn empty_notebook_yields_empty_markdown() {
        let nb = json!({
            "cells": [],
            "metadata": {},
            "nbformat": 4,
            "nbformat_minor": 5,
        });
        let doc = notebook_to_document(&nb);
        assert!(doc.markdown.is_empty());
        assert!(doc.title.is_none());
    }

    #[test]
    fn markdown_cells_pass_through_verbatim() {
        let nb = json!({
            "cells": [
                {"cell_type": "markdown", "source": "# Hello\n\nworld"},
            ],
        });
        let doc = notebook_to_document(&nb);
        assert_eq!(doc.markdown, "# Hello\n\nworld");
    }

    #[test]
    fn source_can_be_array_of_lines() {
        // The on-disk form is usually an array of strings, one per
        // line — diff-friendly. Each line keeps its trailing \n.
        let nb = json!({
            "cells": [
                {"cell_type": "markdown", "source": ["# Hello\n", "\n", "world"]},
            ],
        });
        let doc = notebook_to_document(&nb);
        assert_eq!(doc.markdown, "# Hello\n\nworld");
    }

    #[test]
    fn code_cells_get_fenced_blocks_with_language_hint() {
        let nb = json!({
            "cells": [
                {"cell_type": "code", "source": "print('hi')"},
            ],
            "metadata": {
                "kernelspec": {"language": "python", "display_name": "Python 3"},
            },
        });
        let doc = notebook_to_document(&nb);
        assert_eq!(doc.markdown, "```python\nprint('hi')\n```");
        assert_eq!(
            doc.metadata.get("kernel_language").map(String::as_str),
            Some("python")
        );
        assert_eq!(
            doc.metadata.get("kernel_display_name").map(String::as_str),
            Some("Python 3")
        );
    }

    #[test]
    fn code_cells_without_language_use_unhinted_fence() {
        let nb = json!({
            "cells": [
                {"cell_type": "code", "source": "let x = 1;"},
            ],
        });
        let doc = notebook_to_document(&nb);
        assert_eq!(doc.markdown, "```\nlet x = 1;\n```");
        assert!(!doc.metadata.contains_key("kernel_language"));
    }

    #[test]
    fn language_info_falls_back_when_kernelspec_missing() {
        let nb = json!({
            "cells": [
                {"cell_type": "code", "source": "SELECT 1;"},
            ],
            "metadata": {
                "language_info": {"name": "sql"},
            },
        });
        let doc = notebook_to_document(&nb);
        assert!(doc.markdown.starts_with("```sql\n"));
    }

    #[test]
    fn empty_cells_are_skipped() {
        let nb = json!({
            "cells": [
                {"cell_type": "markdown", "source": "first"},
                {"cell_type": "code", "source": "   "},
                {"cell_type": "markdown", "source": ""},
                {"cell_type": "markdown", "source": "second"},
            ],
        });
        let doc = notebook_to_document(&nb);
        // Whitespace-only and empty cells get filtered, but real
        // content cells are joined with a single blank line.
        assert_eq!(doc.markdown, "first\n\nsecond");
    }

    #[test]
    fn raw_cells_pass_through_verbatim() {
        let nb = json!({
            "cells": [
                {"cell_type": "raw", "source": "<svg>...</svg>"},
            ],
        });
        let doc = notebook_to_document(&nb);
        assert_eq!(doc.markdown, "<svg>...</svg>");
    }

    #[test]
    fn unknown_cell_types_emit_as_opaque_text() {
        // Forward-compat: future nbformat versions might add new
        // cell types. We over-include rather than silently drop.
        let nb = json!({
            "cells": [
                {"cell_type": "future-thing", "source": "preserve me"},
            ],
        });
        let doc = notebook_to_document(&nb);
        assert_eq!(doc.markdown, "preserve me");
    }

    #[test]
    fn malformed_json_returns_typed_error() {
        let result = IpynbExtractor.extract_bytes(b"{ not json", "ipynb");
        assert!(matches!(result, Err(Error::ParseError(_))));
    }

    #[test]
    fn missing_file_returns_io_error() {
        let result = IpynbExtractor.extract(std::path::Path::new("/nonexistent.ipynb"));
        assert!(matches!(result, Err(Error::Io(_))));
    }

    #[test]
    fn title_surfaces_when_metadata_has_it() {
        let nb = json!({
            "cells": [{"cell_type": "markdown", "source": "body"}],
            "metadata": {"title": "My Notebook"},
        });
        let doc = notebook_to_document(&nb);
        assert_eq!(doc.title.as_deref(), Some("My Notebook"));
    }
}
