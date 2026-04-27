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
/// ## OCR fallback (scanned + mixed-content PDFs)
///
/// Pdfium can't extract text from image-only (scanned) pages — the
/// underlying engine sees no text objects, only embedded images.
/// To handle that case, attach an OCR extractor at construction via
/// [`with_ocr_fallback`](Self::with_ocr_fallback). `PdfiumExtractor`
/// then operates per-page: pages whose pdfium text extraction comes
/// back empty (`text.trim().is_empty()`) get rendered to temporary
/// PNGs and routed through the fallback extractor; pages with text
/// pass through unchanged. This handles both the fully-scanned case
/// AND the common mixed-content case (e.g. a text body with one
/// scanned cover or signature page).
///
/// When ANY page goes through OCR, the output switches to a
/// `## Page N`-headed layout so OCR'd pages are visually
/// distinguishable and downstream readers can cite by page. Pure
/// text-only PDFs keep the simpler blank-line-between-pages layout
/// for backward compat with v0.2–v0.5.4.
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

    /// Render only the specified page indices (0-based) to PNGs in
    /// `out_dir`. Used by the v0.5.5 mixed-content OCR path so we
    /// don't burn render time on pages that already extracted clean
    /// text. `indices` is normalised internally (sorted + deduped);
    /// returned PNGs are in ascending page order, with filenames
    /// `page-NNNN.png` carrying the 1-based page number.
    pub fn render_pages_subset_to_pngs(
        &self,
        path: &Path,
        indices: &[usize],
        out_dir: &Path,
    ) -> Result<Vec<std::path::PathBuf>> {
        let path_str = path.to_str().ok_or_else(|| {
            Error::ParseError(format!("PDF path is not valid UTF-8: {}", path.display()))
        })?;
        let pdf_doc = self
            .pdfium
            .load_pdf_from_file(path_str, None)
            .map_err(|e| Error::ParseError(format!("pdfium failed to open {path_str}: {e}")))?;

        // Sort + dedupe so the returned Vec has a predictable
        // (ascending-page-number) order regardless of how the caller
        // assembled `indices`.
        let mut wanted: Vec<usize> = indices.to_vec();
        wanted.sort_unstable();
        wanted.dedup();

        let render_config = PdfRenderConfig::new().scale_page_by_factor(self.ocr_render_scale);
        let mut pngs = Vec::with_capacity(wanted.len());
        for (idx, page) in pdf_doc.pages().iter().enumerate() {
            if wanted.binary_search(&idx).is_err() {
                continue;
            }
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
        let pdf_doc = self
            .pdfium
            .load_pdf_from_file(path_str, None)
            .map_err(|e| Error::ParseError(format!("pdfium failed to open {path_str}: {e}")))?;

        // Per-page text extraction. Holding the per-page Vec (rather
        // than pre-joining) lets us route just the empty pages through
        // OCR if a fallback is configured — the v0.5.5 mixed-content
        // path. v0.5.0–v0.5.4 only triggered OCR when the WHOLE
        // document came back empty, which missed partially-scanned
        // PDFs (e.g. text body with one scanned cover page).
        // pdfium-render's page count returns i32 (PDF spec: pages are
        // u31-bounded); negative values would be a pdfium bug, so
        // saturate at 0 rather than carry the sign-loss risk into our
        // Vec capacity.
        let page_count = usize::try_from(pdf_doc.pages().len()).unwrap_or(0);
        let mut pages: Vec<String> = Vec::with_capacity(page_count);
        for (idx, page) in pdf_doc.pages().iter().enumerate() {
            let text = page.text().map_err(|e| {
                Error::ParseError(format!("page {idx} text extraction failed: {e}"))
            })?;
            pages.push(text.all());
        }

        let (title, mut metadata) = Self::extract_metadata(&pdf_doc);
        // Drop the PdfDocument before we re-open it inside
        // `render_pages_subset_to_pngs` — pdfium-render's thread_safe
        // mode tolerates concurrent loads, but releasing this handle
        // first keeps memory tidy on long PDFs.
        drop(pdf_doc);

        let empty_indices: Vec<usize> = pages
            .iter()
            .enumerate()
            .filter(|(_, t)| t.trim().is_empty())
            .map(|(i, _)| i)
            .collect();

        let any_ocred = !empty_indices.is_empty() && self.ocr_fallback.is_some();

        if any_ocred {
            // unwrap is safe — `any_ocred` requires `is_some()` above.
            let ocr = self.ocr_fallback.as_ref().unwrap().as_ref();
            let temp = tempfile::tempdir().map_err(|e| {
                Error::ParseError(format!(
                    "could not create tempdir for PDF→OCR fallback: {e}"
                ))
            })?;
            let pngs = self.render_pages_subset_to_pngs(path, &empty_indices, temp.path())?;

            for (vec_idx, &page_idx) in empty_indices.iter().enumerate() {
                let png = &pngs[vec_idx];
                let page_doc = ocr.extract(png).map_err(|e| {
                    Error::ParseError(format!(
                        "OCR failed on rendered page {} ({}): {e}",
                        page_idx + 1,
                        png.display()
                    ))
                })?;
                pages[page_idx] = page_doc.markdown;
            }

            metadata.insert(
                "extractor_chain".into(),
                format!("pdfium-render → {}", ocr.name()),
            );
            metadata.insert("pages_ocred".into(), empty_indices.len().to_string());
        }

        // Format the final markdown. When OCR was involved we wrap
        // each page in a `## Page N` heading so downstream readers
        // can cite by page (and so OCR'd pages are visually
        // distinguishable in mixed-content output). For pure-text
        // PDFs we keep the simpler blank-line-between-pages layout
        // that v0.2–v0.5.4 used — preserves backward compat for
        // callers whose snapshots / tests pin that shape.
        let markdown = if any_ocred {
            let mut out = String::new();
            for (idx, page_text) in pages.iter().enumerate() {
                let trimmed = page_text.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if !out.is_empty() {
                    out.push_str("\n\n");
                }
                let _ = write!(out, "## Page {}\n\n{trimmed}", idx + 1);
            }
            out
        } else {
            let mut out = String::new();
            for (idx, page_text) in pages.iter().enumerate() {
                if idx > 0 {
                    out.push_str("\n\n");
                }
                out.push_str(page_text);
            }
            out
        };

        Ok(Document {
            markdown,
            title,
            metadata,
        })
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

    // The OCR-fallback tests below are only compiled on platforms
    // that have a platform OCR backend — Linux without an OCR feature
    // can't satisfy the assertions, and trying to write a
    // uniform-shape test with a panic-typed `let ocr` confused
    // clippy's unreachable_code / unused_variables lints under -D
    // warnings. Cleaner to simply not generate the tests on
    // unsupported targets.

    #[cfg(all(
        feature = "ocr-platform",
        any(target_os = "macos", target_os = "windows")
    ))]
    #[test]
    #[ignore = "requires libpdfium AND a mixed-content PDF in tests/fixtures/mixed-content.pdf"]
    fn mixed_content_pdf_ocrs_only_empty_pages() {
        // Skipped by default. To run on macOS:
        //   cargo test --features "pdf ocr-platform" -- --ignored \
        //     mixed_content_pdf_ocrs_only_empty_pages
        // Drop a 2+ page PDF with at least one text-bearing page AND
        // at least one scanned (image-only) page at
        //   tests/fixtures/mixed-content.pdf
        // The v0.5.5 contract: pages with text pass through pdfium,
        // empty pages get OCR'd, and `pages_ocred` reports a count
        // strictly less than the total page count.
        #[cfg(target_os = "macos")]
        let ocr: Box<dyn Extractor> = Box::new(crate::ocr_macos::VisionOcrExtractor::new());
        #[cfg(target_os = "windows")]
        let ocr: Box<dyn Extractor> = Box::new(crate::ocr_windows::WindowsOcrExtractor::new());

        let extractor = PdfiumExtractor::new()
            .expect("libpdfium not available")
            .with_ocr_fallback(ocr);
        let doc = extractor
            .extract(std::path::Path::new("tests/fixtures/mixed-content.pdf"))
            .expect("extraction failed");

        let pages_ocred: usize = doc
            .metadata
            .get("pages_ocred")
            .and_then(|s| s.parse().ok())
            .expect("pages_ocred metadata should be set when OCR fallback fires");
        assert!(
            pages_ocred >= 1,
            "expected at least one OCR'd page in a mixed-content PDF"
        );
        // The "## Page N" heading layout activates whenever any page
        // went through OCR, so it should appear in mixed output.
        assert!(
            doc.markdown.contains("## Page "),
            "expected `## Page N` heading in mixed-content output"
        );
    }

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
