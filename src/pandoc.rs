//! Pandoc-backed extraction for DOCX, PPTX, EPUB, RTF, ODT, LaTeX, HTML.
//!
//! Spawns the [`pandoc`](https://pandoc.org) binary per file with a
//! stdin/stdout markdown protocol — no Python interpreter, no per-call
//! interpreter cold-start. Pandoc itself is a single static Haskell
//! binary (~150 MB on disk) that consumers bundle alongside their app.
//!
//! Why Pandoc for these formats specifically: 15+ years of polish,
//! gold-standard quality on DOCX/PPTX/EPUB/RTF/ODT/LaTeX, the same
//! engine the academic publishing pipeline runs on. Trying to
//! reproduce that quality in pure Rust would take years.
//!
//! ## Runtime requirement: `pandoc` binary
//!
//! mdkit's `pandoc` feature shells out to a `pandoc` binary at
//! runtime. Two ways to provide one:
//!
//! - **System install** — install Pandoc via your package manager
//!   (`brew install pandoc`, `apt install pandoc`, `choco install
//!   pandoc`), then call [`PandocExtractor::new`] to find it on
//!   PATH automatically.
//! - **Bundled with your app** — ship the static `pandoc` binary
//!   alongside your application binary, then call
//!   [`PandocExtractor::with_binary`] with the absolute path. This
//!   is the path Tauri / Iced / similar apps usually take.
//!
//! Pre-built static Pandoc binaries are available from the official
//! [Pandoc releases](https://github.com/jgm/pandoc/releases) for all
//! major platforms.
//!
//! ## What this extractor does NOT do
//!
//! - **No PDF input.** Pandoc deliberately doesn't read PDFs; use
//!   the [`pdf`](crate::pdf) backend (Pdfium) for that. mdkit's
//!   [`Engine`](crate::Engine) dispatches PDF and Pandoc-formats to
//!   the right backend automatically.
//! - **No XLSX/CSV.** Use the future `calamine` and `csv` backends.
//! - **No image OCR.** Use the future `ocr-platform` / `ocr-onnx`
//!   backends.
//! - **No persistent server mode (yet).** Each `extract` call spawns
//!   a fresh Pandoc process (~50ms cold-start). Pandoc has a server
//!   mode (`pandoc --server`) that would amortize startup; lands as
//!   an opt-in optimization in a later release.

use crate::{Document, Error, Extractor, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Map of supported file extensions → Pandoc `--from` format names.
/// Pandoc supports many more readers; we expose only the ones whose
/// output makes sense for indexing/AI grounding.
const SUPPORTED: &[(&str, &str)] = &[
    ("docx", "docx"),
    ("pptx", "pptx"),
    ("epub", "epub"),
    ("rtf", "rtf"),
    ("odt", "odt"),
    ("tex", "latex"),
    ("latex", "latex"),
    ("html", "html"),
    ("htm", "html"),
];

/// Just the extension list, derived from `SUPPORTED` at compile time.
/// Kept as a separate constant so [`Extractor::extensions`] can return
/// a `&'static [&'static str]` without runtime allocation.
const EXTENSIONS: &[&str] = &[
    "docx", "pptx", "epub", "rtf", "odt", "tex", "latex", "html", "htm",
];

/// Pandoc-backed extractor. Construct via [`PandocExtractor::new`]
/// (locates `pandoc` on PATH) or [`PandocExtractor::with_binary`]
/// (uses an explicit binary path — preferred when shipping pandoc
/// alongside your app).
pub struct PandocExtractor {
    binary: PathBuf,
}

impl PandocExtractor {
    /// Locate `pandoc` on the system PATH and verify it's runnable.
    /// Returns [`Error::MissingDependency`](crate::Error::MissingDependency)
    /// if `pandoc` is not found or doesn't respond to `--version`.
    pub fn new() -> Result<Self> {
        Self::with_binary("pandoc")
    }

    /// Use a specific pandoc binary. The path can be absolute (when
    /// shipping pandoc next to your app) or a bare command name
    /// (which the OS resolves via PATH). The constructor verifies
    /// the binary is executable and responds to `--version`; it
    /// returns [`Error::MissingDependency`](crate::Error::MissingDependency)
    /// otherwise.
    pub fn with_binary(binary: impl Into<PathBuf>) -> Result<Self> {
        let binary = binary.into();
        let result = Command::new(&binary).arg("--version").output();
        match result {
            Ok(output) if output.status.success() => Ok(Self { binary }),
            Ok(output) => Err(Error::MissingDependency {
                name: "pandoc".into(),
                details: format!(
                    "{} --version exited {:?}: {}",
                    binary.display(),
                    output.status.code(),
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            }),
            Err(e) => Err(Error::MissingDependency {
                name: "pandoc".into(),
                details: format!("could not execute {}: {e}", binary.display()),
            }),
        }
    }

    /// Returns the path to the pandoc binary in use. Useful for
    /// diagnostic output and audit logging.
    pub fn binary(&self) -> &Path {
        &self.binary
    }

    /// Returns the Pandoc `--from` format name for an extension, or
    /// `None` if this extractor doesn't claim it. Used by
    /// [`Extractor::extract`] to dispatch the right reader; exposed
    /// publicly so callers can pre-check whether a given file is
    /// supported.
    #[must_use]
    pub fn pandoc_from(ext: &str) -> Option<&'static str> {
        SUPPORTED
            .iter()
            .find(|(e, _)| *e == ext)
            .map(|(_, fmt)| *fmt)
    }
}

impl Extractor for PandocExtractor {
    fn extensions(&self) -> &[&'static str] {
        EXTENSIONS
    }

    fn name(&self) -> &'static str {
        "pandoc"
    }

    fn extract(&self, path: &Path) -> Result<Document> {
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .map(str::to_ascii_lowercase)
            .ok_or_else(|| Error::ParseError(format!("no file extension on {}", path.display())))?;
        let from = Self::pandoc_from(&ext).ok_or_else(|| {
            Error::UnsupportedFormat(format!("pandoc backend does not handle .{ext}"))
        })?;

        let path_str = path.to_str().ok_or_else(|| {
            Error::ParseError(format!("path is not valid UTF-8: {}", path.display()))
        })?;

        // gfm = GitHub-Flavored Markdown — most consumer-friendly
        // markdown dialect with table + strikethrough + task-list
        // support. Switch to `commonmark` if downstream consumers
        // prefer pure CommonMark.
        let output = Command::new(&self.binary)
            .args(["--from", from, "--to", "gfm", path_str])
            .output()
            .map_err(|e| Error::SidecarFailure {
                name: "pandoc".into(),
                code: None,
                stderr: format!("failed to spawn {}: {e}", self.binary.display()),
            })?;

        if !output.status.success() {
            return Err(Error::SidecarFailure {
                name: "pandoc".into(),
                code: output.status.code(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }

        Ok(Document {
            markdown: String::from_utf8_lossy(&output.stdout).into_owned(),
            title: None,
            metadata: HashMap::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Trait-surface tests — don't require a real pandoc binary.

    /// Stand-in that mirrors `PandocExtractor`'s static surface. Used
    /// when we need to verify trait behavior without touching the
    /// filesystem or spawning subprocesses.
    struct FakePandoc;
    impl Extractor for FakePandoc {
        fn extensions(&self) -> &[&'static str] {
            EXTENSIONS
        }
        fn extract(&self, _: &Path) -> Result<Document> {
            unreachable!("FakePandoc only used for trait-surface tests")
        }
        fn name(&self) -> &'static str {
            "pandoc"
        }
    }

    #[test]
    fn covers_expected_office_formats() {
        let exts = FakePandoc.extensions();
        for required in ["docx", "pptx", "epub", "rtf", "odt", "tex", "html"] {
            assert!(
                exts.contains(&required),
                "expected pandoc to handle .{required}, got {exts:?}"
            );
        }
    }

    #[test]
    fn name_identifies_backend() {
        assert_eq!(FakePandoc.name(), "pandoc");
    }

    #[test]
    fn pandoc_from_maps_extensions_to_reader_names() {
        assert_eq!(PandocExtractor::pandoc_from("docx"), Some("docx"));
        assert_eq!(PandocExtractor::pandoc_from("tex"), Some("latex"));
        assert_eq!(PandocExtractor::pandoc_from("htm"), Some("html"));
        assert_eq!(PandocExtractor::pandoc_from("pdf"), None);
        assert_eq!(PandocExtractor::pandoc_from("xyz"), None);
    }

    #[test]
    fn missing_pandoc_returns_typed_error() {
        // Trait-surface guarantee: `with_binary` returns a typed
        // `Error::MissingDependency` (not a panic) when the binary
        // isn't there. We verify with a guaranteed-bad path.
        let result = PandocExtractor::with_binary("/nonexistent-pandoc-path");
        assert!(matches!(
            result,
            Err(Error::MissingDependency { name, .. }) if name == "pandoc"
        ));
    }

    #[test]
    #[ignore = "requires `pandoc` on PATH"]
    fn extracts_a_real_html_file() {
        use std::io::Write;
        // Skipped by default. Run with `cargo test --features pandoc -- --ignored`
        // after ensuring pandoc is installed (`brew install pandoc` etc.).
        // We use HTML as the test format because it's trivial to
        // construct an HTML fixture inline without a separate binary
        // file in the repo.
        let extractor = PandocExtractor::new().expect("pandoc not on PATH");
        let mut tmp = tempfile::Builder::new().suffix(".html").tempfile().unwrap();
        write!(tmp, "<html><body><h1>Hello</h1><p>World</p></body></html>").unwrap();
        tmp.flush().unwrap();

        let doc = extractor.extract(tmp.path()).expect("extraction failed");
        // gfm output for an h1 + paragraph should contain a `# Hello`
        // line and a `World` line. We don't assert the exact format
        // (Pandoc adjusts whitespace across versions) — just that
        // both pieces of text survived the round-trip.
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
}
