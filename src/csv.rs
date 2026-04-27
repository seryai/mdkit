//! CSV / TSV extraction via the [`csv`](https://crates.io/crates/csv) crate.
//!
//! Auto-detects delimiter from the file extension: `.tsv` → tab,
//! everything else → comma. Output is a single GitHub-Flavored
//! Markdown table with the first row treated as the header row
//! (the CSV convention).
//!
//! For Sery-style indexing/AI-grounding use cases, this gives the
//! consumer a structured, searchable text representation. For
//! consumers that need the raw CSV bytes, the file is on disk
//! anyway — mdkit's job is to produce markdown.

use crate::{Document, Error, Extractor, Result};
use std::path::Path;

/// CSV / TSV extractor backed by the `csv` crate. Construct via
/// [`CsvExtractor::new`] — there's no per-instance state.
#[derive(Default)]
pub struct CsvExtractor;

impl CsvExtractor {
    /// Construct an extractor. Cannot fail.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Pick the delimiter byte to use for a given extension. `.tsv`
    /// gets tab; everything else gets comma. Exposed publicly so
    /// callers can reuse the heuristic in their own code.
    #[must_use]
    pub fn delimiter_for(ext: &str) -> u8 {
        if ext.eq_ignore_ascii_case("tsv") {
            b'\t'
        } else {
            b','
        }
    }
}

impl Extractor for CsvExtractor {
    fn extensions(&self) -> &[&'static str] {
        &["csv", "tsv"]
    }

    fn name(&self) -> &'static str {
        "csv"
    }

    fn extract(&self, path: &Path) -> Result<Document> {
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        let delimiter = Self::delimiter_for(&ext);

        let mut reader = ::csv::ReaderBuilder::new()
            .delimiter(delimiter)
            .has_headers(false) // we want raw rows including the first
            .flexible(true) // tolerate ragged rows
            .from_path(path)
            .map_err(|e| Error::ParseError(format!("csv open failed: {e}")))?;

        let mut markdown = String::new();
        let mut rows = reader.records();

        // Header row — the CSV convention. If the file is empty,
        // emit an empty document rather than erroring.
        let Some(first) = rows.next() else {
            return Ok(Document::new(""));
        };
        let header = first.map_err(|e| Error::ParseError(format!("csv parse error: {e}")))?;
        let col_count = header.len();

        markdown.push('|');
        for cell in &header {
            markdown.push(' ');
            markdown.push_str(&escape_cell(cell));
            markdown.push_str(" |");
        }
        markdown.push('\n');

        markdown.push('|');
        for _ in 0..col_count {
            markdown.push_str(" --- |");
        }
        markdown.push('\n');

        for record_result in rows {
            let record =
                record_result.map_err(|e| Error::ParseError(format!("csv parse error: {e}")))?;
            markdown.push('|');
            // Normalize to the header column count to keep the table
            // well-formed even when the source has ragged rows.
            for col_idx in 0..col_count {
                let cell = record.get(col_idx).unwrap_or("");
                markdown.push(' ');
                markdown.push_str(&escape_cell(cell));
                markdown.push_str(" |");
            }
            markdown.push('\n');
        }

        Ok(Document {
            markdown,
            title: None,
            metadata: std::collections::HashMap::new(),
        })
    }
}

/// Escape a cell value for a markdown table cell. Mirrors the
/// helper in `crate::calamine` — kept private to each module so
/// neither has to depend on the other.
fn escape_cell(s: &str) -> String {
    s.replace('|', "&#124;").replace('\n', " ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn handles_csv_and_tsv_extensions() {
        assert_eq!(CsvExtractor.extensions(), &["csv", "tsv"]);
    }

    #[test]
    fn name_identifies_backend() {
        assert_eq!(CsvExtractor.name(), "csv");
    }

    #[test]
    fn delimiter_picks_tab_for_tsv() {
        assert_eq!(CsvExtractor::delimiter_for("tsv"), b'\t');
        assert_eq!(CsvExtractor::delimiter_for("TSV"), b'\t');
        assert_eq!(CsvExtractor::delimiter_for("csv"), b',');
        assert_eq!(CsvExtractor::delimiter_for(""), b',');
    }

    #[test]
    fn extracts_csv_to_markdown_table() {
        let mut tmp = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
        writeln!(tmp, "Name,Email,Age").unwrap();
        writeln!(tmp, "Alice,alice@example.com,30").unwrap();
        writeln!(tmp, "Bob,bob@example.com,25").unwrap();
        tmp.flush().unwrap();

        let doc = CsvExtractor.extract(tmp.path()).unwrap();
        assert!(doc.markdown.contains("| Name | Email | Age |"));
        assert!(doc.markdown.contains("| --- | --- | --- |"));
        assert!(doc.markdown.contains("| Alice | alice@example.com | 30 |"));
        assert!(doc.markdown.contains("| Bob | bob@example.com | 25 |"));
    }

    #[test]
    fn extracts_tsv_with_tab_delimiter() {
        let mut tmp = tempfile::Builder::new().suffix(".tsv").tempfile().unwrap();
        writeln!(tmp, "col1\tcol2").unwrap();
        writeln!(tmp, "v1\tv2").unwrap();
        tmp.flush().unwrap();

        let doc = CsvExtractor.extract(tmp.path()).unwrap();
        assert!(doc.markdown.contains("| col1 | col2 |"));
        assert!(doc.markdown.contains("| v1 | v2 |"));
    }

    #[test]
    fn empty_file_yields_empty_document() {
        let tmp = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
        let doc = CsvExtractor.extract(tmp.path()).unwrap();
        assert!(doc.is_empty());
    }

    #[test]
    fn pipes_in_cell_values_get_escaped() {
        let mut tmp = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
        writeln!(tmp, "field").unwrap();
        writeln!(tmp, "\"a|b\"").unwrap();
        tmp.flush().unwrap();

        let doc = CsvExtractor.extract(tmp.path()).unwrap();
        assert!(
            doc.markdown.contains("a&#124;b"),
            "expected pipe escape in: {:?}",
            doc.markdown
        );
    }

    #[test]
    fn ragged_rows_get_padded_to_header_width() {
        let mut tmp = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
        writeln!(tmp, "a,b,c").unwrap();
        writeln!(tmp, "1,2").unwrap(); // missing third column
        tmp.flush().unwrap();

        let doc = CsvExtractor.extract(tmp.path()).unwrap();
        // Row should still have 3 cells, with the third being empty.
        assert!(
            doc.markdown.contains("| 1 | 2 |  |"),
            "expected ragged row to be padded: {:?}",
            doc.markdown
        );
    }
}
