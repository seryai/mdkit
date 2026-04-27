//! Spreadsheet text extraction via [`calamine`](https://crates.io/crates/calamine).
//!
//! `calamine` is the Rust ecosystem standard for reading Excel
//! workbooks — fast, pure Rust, no Office runtime required. mdkit
//! wraps it as one extractor that handles the common spreadsheet
//! formats: XLSX, XLS, XLSB, XLSM, ODS.
//!
//! Output shape:
//! - One markdown section per worksheet, with the sheet name as a
//!   `## ` heading.
//! - Each sheet's data is rendered as a markdown table with the
//!   first row treated as the header row (standard heuristic; if a
//!   sheet doesn't have a header row, the data still renders — just
//!   with a row that looks like a header). This is the right call
//!   for indexing/AI-grounding use cases where the user just needs
//!   the text to be searchable.
//! - Empty sheets emit just the heading + a "(empty)" note so the
//!   structure of the workbook is visible.
//!
//! Date/time cells are rendered as their ISO-8601 string form.
//! Formula cells render the cached value (calamine's default
//! behavior).

use crate::{Document, Error, Extractor, Result};
use calamine::{open_workbook_auto, Reader};
use std::fmt::Write as _;
use std::path::Path;

/// Spreadsheet extractor backed by `calamine`. Construct via
/// [`CalamineExtractor::new`] — there's no per-instance state, so
/// the constructor is infallible.
#[derive(Default)]
pub struct CalamineExtractor;

impl CalamineExtractor {
    /// Construct an extractor. Cannot fail.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Extractor for CalamineExtractor {
    fn extensions(&self) -> &[&'static str] {
        &["xlsx", "xls", "xlsb", "xlsm", "ods"]
    }

    fn name(&self) -> &'static str {
        "calamine"
    }

    fn extract(&self, path: &Path) -> Result<Document> {
        let mut workbook = open_workbook_auto(path)
            .map_err(|e| Error::ParseError(format!("calamine open failed: {e}")))?;

        let mut markdown = String::new();
        let sheet_names: Vec<String> = workbook.sheet_names().clone();

        for (sheet_idx, sheet_name) in sheet_names.iter().enumerate() {
            if sheet_idx > 0 {
                markdown.push_str("\n\n");
            }
            markdown.push_str("## ");
            markdown.push_str(sheet_name);
            markdown.push_str("\n\n");

            match workbook.worksheet_range(sheet_name) {
                Ok(range) if range.is_empty() => {
                    markdown.push_str("(empty)\n");
                }
                Ok(range) => {
                    render_range_as_table(&range, &mut markdown);
                }
                Err(e) => {
                    let _ = writeln!(markdown, "(could not read sheet: {e})");
                }
            }
        }

        Ok(Document {
            markdown,
            title: None,
            metadata: std::collections::HashMap::new(),
        })
    }
}

/// Render a calamine `Range` as a GitHub-Flavored Markdown table.
/// The first row is treated as headers; subsequent rows as data.
/// Pipe characters in cell values are escaped to keep the table
/// well-formed.
fn render_range_as_table(range: &calamine::Range<calamine::Data>, out: &mut String) {
    let mut rows = range.rows();

    // Header row — calamine returns &[Data] per row.
    let Some(header) = rows.next() else {
        return;
    };
    let col_count = header.len();

    out.push('|');
    for cell in header {
        out.push(' ');
        out.push_str(&escape_cell(&cell.to_string()));
        out.push_str(" |");
    }
    out.push('\n');

    // Separator row — fixed `---` per column.
    out.push('|');
    for _ in 0..col_count {
        out.push_str(" --- |");
    }
    out.push('\n');

    // Data rows.
    for row in rows {
        out.push('|');
        // Pad short rows + truncate long ones to the header column count
        // so the table stays well-formed even when the workbook has
        // ragged data.
        for col_idx in 0..col_count {
            let cell_str = row
                .get(col_idx)
                .map(std::string::ToString::to_string)
                .unwrap_or_default();
            out.push(' ');
            out.push_str(&escape_cell(&cell_str));
            out.push_str(" |");
        }
        out.push('\n');
    }
}

/// Escape a cell value for inclusion in a markdown table:
/// - Replace pipe characters (which would break table structure) with
///   the HTML entity `&#124;`.
/// - Replace newlines (which break a table row) with a space.
fn escape_cell(s: &str) -> String {
    s.replace('|', "&#124;").replace('\n', " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_cover_common_spreadsheet_formats() {
        let ext = CalamineExtractor.extensions();
        for required in ["xlsx", "xls", "ods"] {
            assert!(
                ext.contains(&required),
                "expected calamine to handle .{required}, got {ext:?}"
            );
        }
    }

    #[test]
    fn name_identifies_backend() {
        assert_eq!(CalamineExtractor.name(), "calamine");
    }

    #[test]
    fn escape_cell_handles_pipes_and_newlines() {
        assert_eq!(escape_cell("a|b"), "a&#124;b");
        assert_eq!(escape_cell("a\nb"), "a b");
        assert_eq!(escape_cell("plain text"), "plain text");
    }

    #[test]
    fn missing_file_returns_typed_error() {
        let result = CalamineExtractor.extract(std::path::Path::new("/nonexistent-file-here.xlsx"));
        // calamine returns its own error which we wrap as ParseError.
        // We just verify it's the error type we promised in the trait
        // contract, not a panic.
        assert!(matches!(result, Err(Error::ParseError(_))));
    }
}
