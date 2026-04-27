//! # mdkit — get markdown out of any document.
//!
//! See the [README](https://github.com/mdkit-project/mdkit) for the full
//! design rationale; the short version is: dispatch by file extension to
//! the best backend per format. Pandoc for DOCX/PPTX/EPUB/RTF/ODT/LaTeX,
//! Pdfium for PDF, OS-native APIs for OCR, `calamine` for spreadsheets.
//!
//! ## Quick start
//!
//! ```no_run
//! use mdkit::Engine;
//! use std::path::Path;
//!
//! let engine = Engine::with_defaults();
//! let doc = engine.extract(Path::new("report.pdf"))?;
//! println!("{}", doc.markdown);
//! # Ok::<(), mdkit::Error>(())
//! ```
//!
//! ## Custom extractor
//!
//! Implement [`Extractor`] for your own format and register it on an
//! [`Engine`]:
//!
//! ```
//! use mdkit::{Document, Engine, Extractor, Result};
//! use std::path::Path;
//!
//! struct MyParser;
//!
//! impl Extractor for MyParser {
//!     fn extensions(&self) -> &[&'static str] { &["custom"] }
//!     fn extract(&self, path: &Path) -> Result<Document> {
//!         Ok(Document::new(std::fs::read_to_string(path)?))
//!     }
//! }
//!
//! let mut engine = Engine::new();
//! engine.register(Box::new(MyParser));
//! ```

#![doc(html_root_url = "https://docs.rs/mdkit")]
#![cfg_attr(docsrs, feature(doc_cfg))]

use std::collections::HashMap;
use std::path::Path;

mod error;
pub use error::{Error, Result};

#[cfg(feature = "pdf")]
pub mod pdf;

#[cfg(feature = "calamine")]
pub mod calamine;

#[cfg(feature = "csv")]
pub mod csv;

#[cfg(feature = "html")]
pub mod html;

#[cfg(feature = "pandoc")]
pub mod pandoc;

// Platform-native OCR. Each module is gated by both the
// `ocr-platform` feature AND the matching `target_os`, because the
// underlying FFI deps (objc2-vision on macOS, the `windows` crate on
// Windows) are platform-specific by definition. On Linux with
// `ocr-platform` enabled, neither module compiles — Linux users get
// no platform OCR backend; ONNX-based fallback ships in v0.6 via
// the separate `ocr-onnx` feature.
#[cfg(all(feature = "ocr-platform", target_os = "macos"))]
pub mod ocr_macos;

#[cfg(all(feature = "ocr-platform", target_os = "windows"))]
pub mod ocr_windows;

// ---------------------------------------------------------------------------
// Document — the unit of output
// ---------------------------------------------------------------------------

/// The result of extracting one document. Markdown is always present;
/// title and metadata are best-effort and may be empty depending on the
/// backend.
#[derive(Debug, Clone, Default)]
pub struct Document {
    /// The extracted markdown text.
    pub markdown: String,
    /// Document title if the backend could derive one (DOCX core
    /// properties, PDF metadata, HTML `<title>`, etc.). `None` when
    /// unknown.
    pub title: Option<String>,
    /// Backend-specific metadata. Stable keys are documented per-backend;
    /// callers should treat unknown keys as opaque.
    pub metadata: HashMap<String, String>,
}

impl Document {
    /// Convenience constructor for the common case where you only have
    /// markdown text.
    pub fn new(markdown: impl Into<String>) -> Self {
        Self {
            markdown: markdown.into(),
            title: None,
            metadata: HashMap::new(),
        }
    }

    /// Returns the document's character count. Useful for capping logged
    /// payloads or tracking extraction throughput.
    pub fn len(&self) -> usize {
        self.markdown.chars().count()
    }

    /// Returns true if the extracted markdown is empty.
    pub fn is_empty(&self) -> bool {
        self.markdown.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Extractor — the per-format trait
// ---------------------------------------------------------------------------

/// A backend that knows how to convert one or more file formats to
/// markdown. Implementors register themselves with an [`Engine`].
///
/// `Send + Sync` is required so engines can be shared across threads.
/// All public methods take `&self` so implementors can wrap their
/// internals in `Arc<Mutex<...>>` if they need interior state.
pub trait Extractor: Send + Sync {
    /// Lowercase file extensions this extractor handles, **without**
    /// the leading dot. For example: `&["pdf"]`, `&["docx", "doc"]`.
    fn extensions(&self) -> &[&'static str];

    /// Convert the document at `path` to markdown. Returns
    /// [`Error::Io`] for filesystem failures, [`Error::ParseError`]
    /// for backend-specific failures.
    fn extract(&self, path: &Path) -> Result<Document>;

    /// Convert from in-memory bytes. Default implementation returns
    /// [`Error::UnsupportedOperation`] — backends that can support it
    /// (PDF, HTML) should override.
    fn extract_bytes(&self, _bytes: &[u8], _ext: &str) -> Result<Document> {
        Err(Error::UnsupportedOperation(
            "this extractor does not support in-memory extraction".into(),
        ))
    }

    /// Human-readable backend name, used in error messages and audit
    /// logs (e.g. `"pandoc"`, `"pdfium-render"`, `"calamine"`).
    fn name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }
}

// ---------------------------------------------------------------------------
// Engine — the dispatcher
// ---------------------------------------------------------------------------

/// Dispatches `extract` calls to the registered [`Extractor`] for the
/// file's extension. Construct with [`Engine::new`] for an empty
/// engine, or [`Engine::with_defaults`] to populate the defaults that
/// match enabled feature flags.
pub struct Engine {
    extractors: Vec<Box<dyn Extractor>>,
}

impl Engine {
    /// New engine with no extractors registered. Useful when you want
    /// full control over the backend set.
    pub fn new() -> Self {
        Self {
            extractors: Vec::new(),
        }
    }

    /// New engine with the default backends for the enabled feature
    /// flags. Backends register themselves silently — if a backend
    /// can't initialize (e.g. libpdfium isn't on the system library
    /// path for the `pdf` feature), it's skipped rather than failing
    /// the whole construction. Use [`with_defaults_diagnostic`] if
    /// you want to surface those failures to the user.
    ///
    /// [`with_defaults_diagnostic`]: Self::with_defaults_diagnostic
    pub fn with_defaults() -> Self {
        let (engine, _errors) = Self::with_defaults_diagnostic();
        engine
    }

    /// Like [`with_defaults`](Self::with_defaults) but returns the
    /// list of backend-init errors alongside the engine, so callers
    /// can log "PDF support disabled: libpdfium not found" rather
    /// than silently shipping a degraded experience.
    pub fn with_defaults_diagnostic() -> (Self, Vec<(&'static str, Error)>) {
        // `mut` is conditionally needed: when --no-default-features is
        // set and no optional backends are enabled, neither `engine`
        // nor `errors` ever gets a mutating call. The allow keeps that
        // valid configuration buildable under -D warnings.
        #[allow(unused_mut)]
        let mut engine = Self::new();
        #[allow(unused_mut)]
        let mut errors: Vec<(&'static str, Error)> = Vec::new();

        // Registration order matters: the Engine dispatcher returns
        // the FIRST registered extractor that claims a given file
        // extension. We register cheap in-process Rust backends first
        // so they win over the (heavier) Pandoc sidecar for any
        // overlapping format — most importantly HTML, which both
        // Html2mdExtractor and PandocExtractor handle. Pandoc is the
        // last registered, so it picks up DOCX/PPTX/EPUB/RTF/ODT/LaTeX
        // (which nothing else handles) and ALSO acts as the fallback
        // HTML reader if the `html` feature is disabled.

        #[cfg(feature = "pdf")]
        {
            match crate::pdf::PdfiumExtractor::new() {
                Ok(ext) => {
                    // Wire the platform OCR backend in as a fallback
                    // for scanned (image-only) PDFs. Pdfium can't
                    // extract text from those — without this hop,
                    // PdfiumExtractor returns empty markdown silently.
                    // We construct a SECOND OCR-extractor instance
                    // here (the standalone image-OCR registration is
                    // separate); both are stateless so duplication is
                    // free.
                    #[allow(unused_mut)]
                    let mut ext = ext;
                    #[cfg(all(feature = "ocr-platform", target_os = "macos"))]
                    {
                        ext = ext.with_ocr_fallback(Box::new(
                            crate::ocr_macos::VisionOcrExtractor::new(),
                        ));
                    }
                    #[cfg(all(feature = "ocr-platform", target_os = "windows"))]
                    {
                        ext = ext.with_ocr_fallback(Box::new(
                            crate::ocr_windows::WindowsOcrExtractor::new(),
                        ));
                    }
                    engine.register(Box::new(ext));
                }
                Err(e) => errors.push(("pdf", e)),
            }
        }

        #[cfg(feature = "calamine")]
        {
            engine.register(Box::new(crate::calamine::CalamineExtractor::new()));
        }

        #[cfg(feature = "csv")]
        {
            engine.register(Box::new(crate::csv::CsvExtractor::new()));
        }

        #[cfg(feature = "html")]
        {
            engine.register(Box::new(crate::html::Html2mdExtractor::new()));
        }

        #[cfg(all(feature = "ocr-platform", target_os = "macos"))]
        {
            // Vision is part of macOS — no init failure mode.
            engine.register(Box::new(crate::ocr_macos::VisionOcrExtractor::new()));
        }

        #[cfg(all(feature = "ocr-platform", target_os = "windows"))]
        {
            // Windows.Media.Ocr is part of Windows — no init failure
            // at construction time. (Per-call init may still fail if
            // the user has no OCR-capable language pack installed; we
            // surface that as a typed error from `extract`.)
            engine.register(Box::new(crate::ocr_windows::WindowsOcrExtractor::new()));
        }

        #[cfg(feature = "pandoc")]
        {
            match crate::pandoc::PandocExtractor::new() {
                Ok(ext) => {
                    engine.register(Box::new(ext));
                }
                Err(e) => errors.push(("pandoc", e)),
            }
        }

        (engine, errors)
    }

    /// Register a backend. Multiple backends can claim the same
    /// extension; the first registered wins on dispatch (so you can
    /// override defaults by registering your own extractor first).
    pub fn register(&mut self, extractor: Box<dyn Extractor>) -> &mut Self {
        self.extractors.push(extractor);
        self
    }

    /// Returns the number of registered extractors.
    pub fn len(&self) -> usize {
        self.extractors.len()
    }

    /// Returns true when no extractors are registered.
    pub fn is_empty(&self) -> bool {
        self.extractors.is_empty()
    }

    /// Extract `path` to markdown, dispatching by file extension.
    /// Returns [`Error::UnsupportedFormat`] if no registered extractor
    /// claims the extension.
    pub fn extract(&self, path: &Path) -> Result<Document> {
        let ext = extension_of(path).ok_or_else(|| {
            Error::UnsupportedFormat(format!("no file extension on {}", path.display()))
        })?;
        let extractor = self.find(&ext).ok_or_else(|| {
            Error::UnsupportedFormat(format!("no extractor registered for .{ext}"))
        })?;
        extractor.extract(path)
    }

    /// Same as [`extract`](Self::extract) but takes bytes + an explicit
    /// extension. Backends that don't implement
    /// [`Extractor::extract_bytes`] return
    /// [`Error::UnsupportedOperation`].
    pub fn extract_bytes(&self, bytes: &[u8], ext: &str) -> Result<Document> {
        let lower = ext.trim_start_matches('.').to_ascii_lowercase();
        let extractor = self.find(&lower).ok_or_else(|| {
            Error::UnsupportedFormat(format!("no extractor registered for .{lower}"))
        })?;
        extractor.extract_bytes(bytes, &lower)
    }

    fn find(&self, ext: &str) -> Option<&dyn Extractor> {
        self.extractors
            .iter()
            .find(|e| e.extensions().contains(&ext))
            .map(std::convert::AsRef::as_ref)
    }
}

impl Default for Engine {
    fn default() -> Self {
        Self::with_defaults()
    }
}

fn extension_of(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|os| os.to_str())
        .map(str::to_ascii_lowercase)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    /// A minimal extractor used in unit tests: returns the raw file
    /// content as the markdown body. Stand-in for real backends until
    /// they land per the roadmap.
    struct EchoExtractor {
        exts: &'static [&'static str],
    }

    impl Extractor for EchoExtractor {
        fn extensions(&self) -> &[&'static str] {
            self.exts
        }
        fn extract(&self, path: &Path) -> Result<Document> {
            Ok(Document::new(std::fs::read_to_string(path)?))
        }
        fn extract_bytes(&self, bytes: &[u8], _ext: &str) -> Result<Document> {
            Ok(Document::new(String::from_utf8_lossy(bytes).into_owned()))
        }
    }

    #[test]
    fn empty_engine_rejects_all_files() {
        let engine = Engine::new();
        let f = NamedTempFile::new().unwrap();
        let err = engine.extract(f.path()).unwrap_err();
        assert!(matches!(err, Error::UnsupportedFormat(_)));
    }

    #[test]
    fn dispatches_by_extension() {
        let mut engine = Engine::new();
        engine.register(Box::new(EchoExtractor { exts: &["txt"] }));

        let mut f = tempfile::Builder::new().suffix(".txt").tempfile().unwrap();
        write!(f, "hello world").unwrap();
        f.flush().unwrap();

        let doc = engine.extract(f.path()).unwrap();
        assert_eq!(doc.markdown, "hello world");
    }

    #[test]
    fn extension_match_is_case_insensitive() {
        let mut engine = Engine::new();
        engine.register(Box::new(EchoExtractor { exts: &["pdf"] }));

        let mut f = tempfile::Builder::new().suffix(".PDF").tempfile().unwrap();
        write!(f, "fake pdf").unwrap();
        f.flush().unwrap();

        // Engine should normalize the extension to lowercase before
        // looking up the extractor — `EchoExtractor` registered as "pdf"
        // must still match a file ending ".PDF".
        let doc = engine.extract(f.path()).unwrap();
        assert_eq!(doc.markdown, "fake pdf");
    }

    #[test]
    fn first_registered_extractor_wins() {
        let mut engine = Engine::new();
        engine.register(Box::new(EchoExtractor { exts: &["md"] }));
        // A second extractor for the same extension should be reachable
        // only via direct calls — the dispatcher picks the first match.
        engine.register(Box::new(EchoExtractor { exts: &["md"] }));
        assert_eq!(engine.len(), 2);
    }

    #[test]
    fn extract_bytes_uses_explicit_extension() {
        let mut engine = Engine::new();
        engine.register(Box::new(EchoExtractor { exts: &["html"] }));

        let doc = engine.extract_bytes(b"<p>hi</p>", "html").unwrap();
        assert_eq!(doc.markdown, "<p>hi</p>");

        // Leading dot is tolerated.
        let doc2 = engine.extract_bytes(b"<p>hi</p>", ".html").unwrap();
        assert_eq!(doc2.markdown, "<p>hi</p>");
    }

    #[test]
    fn missing_extension_is_a_clean_error() {
        let engine = Engine::with_defaults();
        let f = tempfile::Builder::new().tempfile().unwrap();
        let err = engine.extract(f.path()).unwrap_err();
        assert!(matches!(err, Error::UnsupportedFormat(_)));
    }

    #[test]
    fn document_helpers_work() {
        let mut doc = Document::new("hello");
        assert_eq!(doc.len(), 5);
        assert!(!doc.is_empty());
        doc.markdown.clear();
        assert!(doc.is_empty());
    }
}
