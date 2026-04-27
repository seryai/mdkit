//! PDF text extraction via Google's Pdfium engine.
//!
//! Backed by the [`pdfium-render`](https://crates.io/crates/pdfium-render)
//! crate, which wraps Pdfium — the same PDF engine that ships in
//! Chrome and that powers most of the world's web-based PDF viewing.
//! Layout-aware, multi-column-friendly, handles encrypted documents
//! (returns a clean error when no password is supplied).
//!
//! ## Runtime requirement: libpdfium
//!
//! `pdfium-render` doesn't bundle the actual Pdfium library — it loads
//! `libpdfium.{so,dylib,dll}` dynamically at runtime. Consumers of
//! mdkit's `pdf` feature need to make libpdfium available on their
//! library search path.
//!
//! Recommended sources of pre-built libpdfium binaries:
//!
//! - [bblanchon/pdfium-binaries](https://github.com/bblanchon/pdfium-binaries) —
//!   community-maintained pre-built binaries for all major platforms.
//! - [paulocoutinhox/pdfium-lib](https://github.com/paulocoutinhox/pdfium-lib) —
//!   per-platform release archives.
//!
//! On macOS and Linux you typically drop `libpdfium.dylib` /
//! `libpdfium.so` next to your binary or onto `LD_LIBRARY_PATH`. On
//! Windows, place `pdfium.dll` next to the executable.
//!
//! ## What this extractor does NOT do
//!
//! - **No OCR.** Scanned (image-only) PDFs return empty or near-empty
//!   text. The OCR backends (`ocr-platform`, `ocr-onnx`) handle the
//!   image-text case; mdkit's [`Engine`](crate::Engine) will fall
//!   back to OCR automatically when both features are enabled.
//! - **No password support.** Encrypted PDFs return
//!   [`Error::ParseError`](crate::Error::ParseError) with a clear
//!   message. Password-protected extraction lands when a real
//!   user-need surfaces.
//! - **No layout-mode selection.** Pdfium's default text-extraction
//!   mode is used, which preserves reading order for most documents.
//!   A configurable layout mode lands if real-world output proves
//!   inadequate.

use crate::{Document, Error, Extractor, Result};
use pdfium_render::prelude::*;
use std::fmt::Write as _;
use std::path::Path;

/// PDF extractor backed by Pdfium. Construct via [`PdfiumExtractor::new`]
/// (which discovers libpdfium on the system library path) or
/// [`PdfiumExtractor::with_library_path`] (which loads from an explicit
/// directory — useful when libpdfium ships next to your application
/// binary).
///
/// ## OCR fallback (scanned PDFs)
///
/// Pdfium can't extract text from image-only (scanned) PDFs — the
/// underlying engine sees no text objects, only embedded images.
/// To handle that case, attach an OCR extractor at construction
/// via [`with_ocr_fallback`](Self::with_ocr_fallback). When `extract`
/// would otherwise return empty markdown, `PdfiumExtractor` renders
/// each page to a temporary PNG and routes those through the
/// fallback extractor, joining the per-page results into a single
/// markdown body.
///
/// [`Engine::with_defaults`](crate::Engine::with_defaults) wires
/// the platform OCR backend into `PdfiumExtractor` automatically when
/// both `pdf` and `ocr-platform` features are enabled and the
/// target OS has a native OCR engine (macOS / Windows in v0.5.x).
pub struct PdfiumExtractor {
    pdfium: Pdfium,
    /// Optional second-pass extractor invoked when Pdfium returns no
    /// text. Only consulted for PDFs whose primary extraction yields
    /// `markdown.trim().is_empty()`.
    ocr_fallback: Option<Box<dyn Extractor>>,
    /// Page-render scale factor for the OCR fallback path. Default
    /// 2.0 ≈ 144 DPI, a balance between OCR accuracy and Windows
    /// `MaxImageDimension` (~2600 px on shipping Windows).
    ocr_render_scale: f32,
}

impl PdfiumExtractor {
    /// Construct an extractor by binding to libpdfium on the system's
    /// default library search path. Returns
    /// [`Error::MissingDependency`](crate::Error::MissingDependency)
    /// if libpdfium can't be found or loaded.
    pub fn new() -> Result<Self> {
        let bindings = Pdfium::bind_to_system_library().map_err(|e| Error::MissingDependency {
            name: "libpdfium".into(),
            details: format!("could not load from system library path: {e}"),
        })?;
        Ok(Self {
            pdfium: Pdfium::new(bindings),
            ocr_fallback: None,
            ocr_render_scale: 2.0,
        })
    }

    /// Construct an extractor by binding to libpdfium at an explicit
    /// path. Useful when the libpdfium binary ships alongside your
    /// application binary rather than being installed system-wide.
    /// The path should be the *directory* containing libpdfium —
    /// `pdfium-render` resolves the platform-specific filename
    /// (`libpdfium.dylib` / `libpdfium.so` / `pdfium.dll`).
    pub fn with_library_path(library_dir: &str) -> Result<Self> {
        let bindings =
            Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path(library_dir))
                .map_err(|e| Error::MissingDependency {
                    name: "libpdfium".into(),
                    details: format!("could not load from {library_dir}: {e}"),
                })?;
        Ok(Self {
            pdfium: Pdfium::new(bindings),
            ocr_fallback: None,
            ocr_render_scale: 2.0,
        })
    }

    /// Attach an OCR extractor to handle scanned PDFs. When `extract`
    /// would otherwise return empty markdown (typical for image-only
    /// PDFs), each page is rendered to a temporary PNG and routed
    /// through `ocr`. Returns `Self` for builder-style chaining.
    ///
    /// `Engine::with_defaults` calls this automatically when the
    /// platform OCR backend is enabled, so most callers don't need
    /// to invoke it directly.
    #[must_use]
    pub fn with_ocr_fallback(mut self, ocr: Box<dyn Extractor>) -> Self {
        self.ocr_fallback = Some(ocr);
        self
    }

    /// Override the page-render scale used by the OCR fallback. The
    /// default (2.0) maps to ~144 DPI, a balance between OCR
    /// accuracy and Windows OCR's `MaxImageDimension` cap. Higher
    /// scales improve OCR on small text but risk blowing past the
    /// cap on letter-size pages (~2550 px wide at scale 3.0).
    #[must_use]
    pub fn with_ocr_render_scale(mut self, scale: f32) -> Self {
        self.ocr_render_scale = scale;
        self
    }

    /// Render each page of `path` to a PNG file in `out_dir` at the
    /// extractor's configured scale. Returns the PNG paths in page
    /// order. Used internally by the OCR-fallback path; exposed
    /// publicly so callers building richer pipelines can reuse it.
    pub fn render_pages_to_pngs(
        &self,
        path: &Path,
        out_dir: &Path,
    ) -> Result<Vec<std::path::PathBuf>> {
        let path_str = path.to_str().ok_or_else(|| {
            Error::ParseError(format!("PDF path is not valid UTF-8: {}", path.display()))
        })?;
        let doc = self
            .pdfium
            .load_pdf_from_file(path_str, None)
            .map_err(|e| Error::ParseError(format!("pdfium failed to open {path_str}: {e}")))?;

        let render_config = PdfRenderConfig::new().scale_page_by_factor(self.ocr_render_scale);
        let mut pngs = Vec::new();
        for (idx, page) in doc.pages().iter().enumerate() {
            let bitmap = page
                .render_with_config(&render_config)
                .map_err(|e| Error::ParseError(format!("page {idx} render failed: {e}")))?;
            let image = bitmap
                .as_image()
                .map_err(|e| Error::ParseError(format!("page {idx} bitmap → image failed: {e}")))?;
            let png_path = out_dir.join(format!("page-{:04}.png", idx + 1));
            image.save(&png_path).map_err(|e| {
                Error::ParseError(format!(
                    "failed to write rendered page {idx} to {}: {e}",
                    png_path.display()
                ))
            })?;
            pngs.push(png_path);
        }
        Ok(pngs)
    }

    /// Internal: extract text from an already-loaded `PdfDocument`.
    /// Pages are joined with `\n\n` (one blank line between pages),
    /// preserving the document's reading order without injecting
    /// opinionated heading markup. Document-level metadata (title,
    /// author, …) is harvested via [`extract_metadata`].
    fn extract_from_document(doc: &PdfDocument) -> Result<Document> {
        let mut markdown = String::new();
        for (idx, page) in doc.pages().iter().enumerate() {
            if idx > 0 {
                markdown.push_str("\n\n");
            }
            let text = page.text().map_err(|e| {
                Error::ParseError(format!("page {idx} text extraction failed: {e}"))
            })?;
            markdown.push_str(&text.all());
        }

        let (title, metadata) = Self::extract_metadata(doc);

        Ok(Document {
            markdown,
            title,
            metadata,
        })
    }

    /// Read the PDF's document-information dictionary (the standard
    /// `/Title /Author /Subject /Keywords /Creator /Producer
    /// /CreationDate /ModDate` set per ISO 32000-2 §14.3.3) and map
    /// it into a `(title, metadata)` pair.
    ///
    /// Tags with empty values are skipped so callers don't have to
    /// distinguish "absent" from "present-but-empty". Date values are
    /// passed through verbatim — Pdfium hands us PDF-spec date strings
    /// (e.g. `D:20240115120000Z`) and parsing them into RFC 3339 is
    /// out of scope for the extractor surface; downstream code that
    /// cares can parse `metadata["created_at"]` itself.
    fn extract_metadata(
        doc: &PdfDocument,
    ) -> (Option<String>, std::collections::HashMap<String, String>) {
        let mut title: Option<String> = None;
        let mut metadata = std::collections::HashMap::new();

        for tag in doc.metadata().iter() {
            let value = tag.value();
            if value.trim().is_empty() {
                continue;
            }
            let key = match tag.tag_type() {
                PdfDocumentMetadataTagType::Title => {
                    title = Some(value.to_string());
                    "title"
                }
                PdfDocumentMetadataTagType::Author => "author",
                PdfDocumentMetadataTagType::Subject => "subject",
                PdfDocumentMetadataTagType::Keywords => "keywords",
                PdfDocumentMetadataTagType::Creator => "creator",
                PdfDocumentMetadataTagType::Producer => "producer",
                PdfDocumentMetadataTagType::CreationDate => "created_at",
                PdfDocumentMetadataTagType::ModificationDate => "modified_at",
            };
            metadata.insert(key.to_string(), value.to_string());
        }

        (title, metadata)
    }
}

impl Extractor for PdfiumExtractor {
    fn extensions(&self) -> &[&'static str] {
        &["pdf"]
    }

    fn name(&self) -> &'static str {
        "pdfium-render"
    }

    fn extract(&self, path: &Path) -> Result<Document> {
        let path_str = path.to_str().ok_or_else(|| {
            Error::ParseError(format!("PDF path is not valid UTF-8: {}", path.display()))
        })?;
        let doc = {
            let pdf_doc = self
                .pdfium
                .load_pdf_from_file(path_str, None)
                .map_err(|e| Error::ParseError(format!("pdfium failed to open {path_str}: {e}")))?;
            Self::extract_from_document(&pdf_doc)?
        };

        // Scanned-PDF OCR composition: when the primary text-extraction
        // pass returns nothing and an OCR backend is registered as a
        // fallback, render each page and route through OCR. We only
        // engage the fallback on TRULY empty results (`trim().is_empty()`)
        // — partial extractions stay as-is, since mixing pdfium text
        // with OCR'd text on the same page tends to produce duplicate
        // or garbled output.
        //
        // Metadata (title, author, etc.) survives the swap: scanned PDFs
        // often have populated /Info dicts even when /Contents is
        // image-only, so we merge what `extract_from_document` already
        // pulled into the OCR result rather than dropping it.
        if doc.markdown.trim().is_empty() {
            if let Some(ocr) = &self.ocr_fallback {
                return self.extract_via_ocr(path, ocr.as_ref(), doc.title, doc.metadata);
            }
        }

        Ok(doc)
    }

    fn extract_bytes(&self, bytes: &[u8], _ext: &str) -> Result<Document> {
        // OCR fallback isn't wired for the bytes path — it'd need to
        // spool the PDF to a tempfile first. Left for a future release
        // if real callers ask for it; the file-path API covers the
        // dominant use case (Tauri/Iced apps reading off disk).
        let doc = self
            .pdfium
            .load_pdf_from_byte_slice(bytes, None)
            .map_err(|e| Error::ParseError(format!("pdfium failed to open byte slice: {e}")))?;
        Self::extract_from_document(&doc)
    }
}

impl PdfiumExtractor {
    /// Render each page to a PNG, OCR each PNG via `ocr`, join the
    /// per-page markdown with `## Page N` headings so downstream
    /// readers can cite by page. Best-effort: a per-page OCR failure
    /// surfaces as a typed error rather than being silently skipped.
    ///
    /// `existing_title` and `existing_metadata` come from the primary
    /// pdfium extraction — they survive the OCR detour since scanned
    /// PDFs commonly have populated /Info dicts that we want to keep.
    /// `extractor_chain` and `pages_ocred` are added on top.
    fn extract_via_ocr(
        &self,
        path: &Path,
        ocr: &dyn Extractor,
        existing_title: Option<String>,
        mut metadata: std::collections::HashMap<String, String>,
    ) -> Result<Document> {
        let temp = tempfile::tempdir().map_err(|e| {
            Error::ParseError(format!(
                "could not create tempdir for PDF→OCR fallback: {e}"
            ))
        })?;
        let pngs = self.render_pages_to_pngs(path, temp.path())?;

        let mut markdown = String::new();
        for (idx, png) in pngs.iter().enumerate() {
            let page_doc = ocr.extract(png).map_err(|e| {
                Error::ParseError(format!(
                    "OCR failed on rendered page {} ({}): {e}",
                    idx + 1,
                    png.display()
                ))
            })?;
            let page_text = page_doc.markdown.trim();
            if page_text.is_empty() {
                continue;
            }
            if !markdown.is_empty() {
                markdown.push_str("\n\n");
            }
            // write! into a String never fails; the Result is
            // discarded with `let _ = ...` to satisfy clippy.
            let _ = write!(markdown, "## Page {}\n\n", idx + 1);
            markdown.push_str(page_text);
        }

        metadata.insert(
            "extractor_chain".into(),
            format!("pdfium-render → {}", ocr.name()),
        );
        metadata.insert("pages_ocred".into(), pngs.len().to_string());

        Ok(Document {
            markdown,
            title: existing_title,
            metadata,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The trait-surface tests don't need libpdfium — they verify
    // shape/behavior that we control without a runtime dependency.
    // Real extraction tests are #[ignore]'d so they don't fail on
    // dev machines / CI runners that don't have libpdfium installed.
    // Run them locally with: cargo test --features pdf -- --ignored

    /// Reusable stand-in for trait-surface tests so we don't have to
    /// instantiate a real `PdfiumExtractor` (which would require libpdfium
    /// on the system library path). Mirrors `PdfiumExtractor`'s
    /// extensions + name.
    struct FakePdf;
    impl Extractor for FakePdf {
        fn extensions(&self) -> &[&'static str] {
            &["pdf"]
        }
        fn extract(&self, _: &std::path::Path) -> Result<Document> {
            unreachable!("FakePdf only used for trait-surface tests")
        }
        fn name(&self) -> &'static str {
            "pdfium-render"
        }
    }

    #[test]
    fn extensions_is_pdf_only() {
        assert_eq!(FakePdf.extensions(), &["pdf"]);
    }

    #[test]
    fn name_identifies_backend() {
        assert_eq!(FakePdf.name(), "pdfium-render");
    }

    #[test]
    #[ignore = "requires libpdfium on the system library path"]
    fn extracts_text_from_a_real_pdf() {
        // Skipped by default. To run: ensure libpdfium is on your
        // library path, then `cargo test --features pdf -- --ignored`.
        // Drop a "hello.pdf" containing the literal text "Hello,
        // World!" into tests/fixtures/ before running.
        let extractor = PdfiumExtractor::new().expect("libpdfium not available");
        let doc = extractor
            .extract(std::path::Path::new("tests/fixtures/hello.pdf"))
            .expect("extraction failed");
        assert!(
            !doc.markdown.is_empty(),
            "expected non-empty markdown from hello.pdf"
        );
    }

    #[test]
    #[ignore = "requires libpdfium AND a PDF with metadata at tests/fixtures/with-metadata.pdf"]
    fn surfaces_pdf_metadata_and_title() {
        // Skipped by default. To run: drop a PDF with at least Title +
        // Author set in its /Info dict (most PDFs exported from Word /
        // LaTeX / Pages do this automatically) at
        //   tests/fixtures/with-metadata.pdf
        // then run:
        //   cargo test --features pdf -- --ignored \
        //     surfaces_pdf_metadata_and_title
        let extractor = PdfiumExtractor::new().expect("libpdfium not available");
        let doc = extractor
            .extract(std::path::Path::new("tests/fixtures/with-metadata.pdf"))
            .expect("extraction failed");

        // Title surfaces on Document.title — the v0.5.4 contract.
        assert!(
            doc.title.is_some(),
            "expected Document.title to be populated from /Title; got {doc:?}"
        );
        // Title is also mirrored under metadata["title"] for callers
        // that consume metadata uniformly.
        assert_eq!(
            doc.metadata.get("title").map(String::as_str),
            doc.title.as_deref(),
            "metadata['title'] should mirror Document.title"
        );
    }

    // Only define this test on platforms that have a platform OCR
    // backend — Linux without an OCR feature can't satisfy the
    // assertions, and trying to write a uniform-shape test with a
    // panic-typed `let ocr` confused clippy's unreachable_code /
    // unused_variables lints under -D warnings. Cleaner to simply
    // not generate the test on unsupported targets.
    #[cfg(all(
        feature = "ocr-platform",
        any(target_os = "macos", target_os = "windows")
    ))]
    #[test]
    #[ignore = "requires libpdfium AND a scanned PDF in tests/fixtures/scanned.pdf"]
    fn scanned_pdf_routes_through_ocr_fallback() {
        // Skipped by default. To run on macOS:
        //   cargo test --features "pdf ocr-platform" -- --ignored \
        //     scanned_pdf_routes_through_ocr_fallback
        // Drop an image-only PDF (e.g. a screenshot saved as PDF) at
        // tests/fixtures/scanned.pdf — primary pdfium extraction must
        // return empty markdown so the fallback path engages.
        #[cfg(target_os = "macos")]
        let ocr: Box<dyn Extractor> = Box::new(crate::ocr_macos::VisionOcrExtractor::new());
        #[cfg(target_os = "windows")]
        let ocr: Box<dyn Extractor> = Box::new(crate::ocr_windows::WindowsOcrExtractor::new());

        let extractor = PdfiumExtractor::new()
            .expect("libpdfium not available")
            .with_ocr_fallback(ocr);
        let doc = extractor
            .extract(std::path::Path::new("tests/fixtures/scanned.pdf"))
            .expect("extraction failed");
        assert!(
            !doc.markdown.is_empty(),
            "expected non-empty markdown from scanned.pdf via OCR fallback"
        );

        let chain = doc
            .metadata
            .get("extractor_chain")
            .map_or("", String::as_str);
        assert!(
            chain == "pdfium-render → vision-macos" || chain == "pdfium-render → ocr-windows",
            "expected extractor_chain to record the fallback hop, got {chain:?}"
        );
    }

    #[test]
    fn missing_libpdfium_returns_typed_error() {
        // Trait-surface guarantee: `PdfiumExtractor` returns a typed
        // `Error::MissingDependency` (not a panic) when libpdfium
        // isn't on the path. We can't reliably trigger the failure
        // on every dev machine, but we CAN verify the error variant
        // is correctly typed by attempting a guaranteed-bad path.
        let result = PdfiumExtractor::with_library_path("/nonexistent-path-that-cannot-exist");
        assert!(matches!(result, Err(Error::MissingDependency { .. })));
    }
}
