//! HTML extraction via [`html2md`](https://crates.io/crates/html2md).
//!
//! Lightweight, pure-Rust HTML→markdown conversion. The output
//! quality is good enough for indexing/AI-grounding use cases — not
//! as polished as Pandoc's HTML reader (Pandoc preserves more edge-
//! case structure) but in-process, dependency-light, and fast.
//!
//! For consumers who want best-in-world HTML conversion quality, the
//! [`pandoc`](crate::pandoc) backend also handles HTML and registers
//! after this one in `Engine::with_defaults()`. Register
//! `PandocExtractor` first if you want it to win for HTML files.

use crate::{Document, Extractor, Result};
use std::path::Path;

#[cfg(test)]
use crate::Error;

/// HTML extractor backed by `html2md`. Construct via
/// [`Html2mdExtractor::new`] — there's no per-instance state.
#[derive(Default)]
pub struct Html2mdExtractor;

impl Html2mdExtractor {
    /// Construct an extractor. Cannot fail.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Extractor for Html2mdExtractor {
    fn extensions(&self) -> &[&'static str] {
        &["html", "htm"]
    }

    fn name(&self) -> &'static str {
        "html2md"
    }

    fn extract(&self, path: &Path) -> Result<Document> {
        let html = std::fs::read_to_string(path)?;
        let markdown = html2md::parse_html(&html);
        Ok(Document {
            markdown,
            title: None,
            metadata: std::collections::HashMap::new(),
        })
    }

    fn extract_bytes(&self, bytes: &[u8], _ext: &str) -> Result<Document> {
        // html2md takes &str; if the bytes aren't UTF-8 we lossy-decode
        // rather than failing — most HTML in the wild is UTF-8 and the
        // ones that aren't are usually valid latin-1, which lossy
        // decoding handles reasonably.
        let html = std::str::from_utf8(bytes).map_or_else(
            |_| String::from_utf8_lossy(bytes).into_owned(),
            std::string::ToString::to_string,
        );
        Ok(Document {
            markdown: html2md::parse_html(&html),
            title: None,
            metadata: std::collections::HashMap::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn handles_html_and_htm_extensions() {
        assert_eq!(Html2mdExtractor.extensions(), &["html", "htm"]);
    }

    #[test]
    fn name_identifies_backend() {
        assert_eq!(Html2mdExtractor.name(), "html2md");
    }

    #[test]
    fn converts_basic_html_to_markdown() {
        let mut tmp = tempfile::Builder::new().suffix(".html").tempfile().unwrap();
        write!(tmp, "<html><body><h1>Hello</h1><p>World</p></body></html>").unwrap();
        tmp.flush().unwrap();

        let doc = Html2mdExtractor.extract(tmp.path()).unwrap();
        assert!(
            doc.markdown.contains("Hello"),
            "expected 'Hello' in output: {:?}",
            doc.markdown
        );
        assert!(
            doc.markdown.contains("World"),
            "expected 'World' in output: {:?}",
            doc.markdown
        );
    }

    #[test]
    fn extract_bytes_works_too() {
        let bytes = b"<h1>From Bytes</h1>";
        let doc = Html2mdExtractor.extract_bytes(bytes, "html").unwrap();
        assert!(
            doc.markdown.contains("From Bytes"),
            "expected 'From Bytes' in output: {:?}",
            doc.markdown
        );
    }

    #[test]
    fn missing_file_returns_io_error() {
        let result =
            Html2mdExtractor.extract(std::path::Path::new("/nonexistent-html-file-here.html"));
        assert!(matches!(result, Err(Error::Io(_))));
    }
}
