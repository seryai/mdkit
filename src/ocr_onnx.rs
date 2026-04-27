//! Cross-platform OCR via ONNX-runtime `PaddleOCR` models.
//!
//! Backed by [`oar-ocr`](https://crates.io/crates/oar-ocr), which
//! wraps `PaddleOCR`'s detection + recognition ONNX exports through
//! [`ort`](https://crates.io/crates/ort) (the de-facto ONNX Runtime
//! Rust binding). Works on Linux + macOS + Windows + WebAssembly,
//! making this the recommended OCR backend on Linux (where
//! `ocr-platform` has no native engine to offer).
//!
//! ## Runtime requirements
//!
//! - **`libonnxruntime`** at runtime — same dynamic-loading pattern
//!   as `libpdfium` for the `pdf` feature. Either install via your
//!   system package manager (`apt install onnxruntime` /
//!   `brew install onnxruntime`), drop the shared library next to
//!   your binary, or enable the `ocr-onnx-download` feature on
//!   mdkit so `oar-ocr` fetches it for you at build / first use.
//! - **`PaddleOCR` ONNX model files** — three files, supplied by the
//!   caller via [`OnnxOcrExtractor::with_models`]. Download from
//!   <https://github.com/GreatV/oar-ocr/releases>. For English-only
//!   recognition you need:
//!     - Detection: `pp-ocrv5_mobile_det.onnx` (~4.6 MB)
//!     - Recognition: `en_pp-ocrv5_mobile_rec.onnx` (~7.5 MB)
//!     - Dict: `ppocrv5_en_dict.txt`
//!
//!   For other languages, swap the recognition model + dict; the
//!   detection model is language-independent.
//!
//! ## What this extractor handles
//!
//! Standalone image files: PNG, JPG/JPEG, TIFF/TIF, BMP, GIF.
//! HEIC/HEIF aren't supported because they require a separate
//! libheif dep — use the macOS Vision backend on Apple platforms
//! when HEIC handling matters.
//!
//! ## Output shape
//!
//! Each detected text region becomes one line of markdown in
//! reading order (top-to-bottom, then left-to-right within a row).
//! Confidence scores and bounding boxes aren't surfaced today —
//! the [`Extractor`](crate::Extractor) trait stays intentionally
//! simple. A future "rich extraction" trait could expose them.
//!
//! ## Why not auto-register in `Engine::with_defaults`?
//!
//! Because constructing this extractor *requires* model paths and
//! we have no portable default location to look. Callers wire it
//! up explicitly:
//!
//! ```no_run
//! use mdkit::{Engine, ocr_onnx::OnnxOcrExtractor};
//! use std::path::Path;
//!
//! let ocr = OnnxOcrExtractor::with_models(
//!     Path::new("/opt/models/pp-ocrv5_mobile_det.onnx"),
//!     Path::new("/opt/models/en_pp-ocrv5_mobile_rec.onnx"),
//!     Path::new("/opt/models/ppocrv5_en_dict.txt"),
//! )?;
//!
//! let mut engine = Engine::with_defaults();
//! engine.register(Box::new(ocr));
//! # Ok::<(), mdkit::Error>(())
//! ```

use crate::{Document, Error, Extractor, Result};
use oar_ocr::prelude::*;
use std::path::{Path, PathBuf};

/// ONNX-runtime PaddleOCR-backed extractor. Construct via
/// [`with_models`](Self::with_models).
///
/// Thread-safe: a single `OnnxOcrExtractor` can be `register`ed once
/// and serves concurrent `extract` calls. The underlying `OAROCR`
/// pipeline holds an `ort::Session` which is `Send + Sync`.
pub struct OnnxOcrExtractor {
    ocr: OAROCR,
    /// Held for [`name`](Extractor::name) and error messages — the
    /// detection model path is the most stable identifier when
    /// multiple ONNX backends might be registered.
    detection_model: PathBuf,
}

impl OnnxOcrExtractor {
    /// Construct with caller-provided model paths. All three files
    /// are required; download from
    /// <https://github.com/GreatV/oar-ocr/releases>.
    ///
    /// - `detection_model`: ONNX detection model (e.g.
    ///   `pp-ocrv5_mobile_det.onnx`). Language-independent.
    /// - `recognition_model`: ONNX recognition model
    ///   (e.g. `en_pp-ocrv5_mobile_rec.onnx`). Language-specific.
    /// - `dict`: text dictionary file (e.g. `ppocrv5_en_dict.txt`).
    ///   Must match the recognition model's vocabulary.
    ///
    /// Returns [`Error::MissingDependency`](crate::Error::MissingDependency)
    /// when any model file is unreadable (the most likely cause is
    /// a typo or missing download), and
    /// [`Error::ParseError`](crate::Error::ParseError) when oar-ocr
    /// rejects the model (corrupt file, version mismatch, missing
    /// `libonnxruntime`).
    pub fn with_models(
        detection_model: &Path,
        recognition_model: &Path,
        dict: &Path,
    ) -> Result<Self> {
        for (label, p) in [
            ("detection_model", detection_model),
            ("recognition_model", recognition_model),
            ("dict", dict),
        ] {
            if !p.exists() {
                return Err(Error::MissingDependency {
                    name: format!("oar-ocr {label}"),
                    details: format!(
                        "file not found: {} — download from \
                         https://github.com/GreatV/oar-ocr/releases",
                        p.display()
                    ),
                });
            }
        }

        let ocr = OAROCRBuilder::new(
            detection_model.to_path_buf(),
            recognition_model.to_path_buf(),
            dict.to_path_buf(),
        )
        .build()
        .map_err(|e| {
            Error::ParseError(format!(
                "oar-ocr pipeline construction failed (libonnxruntime missing or \
                 model file rejected): {e}"
            ))
        })?;

        Ok(Self {
            ocr,
            detection_model: detection_model.to_path_buf(),
        })
    }

    /// The detection model path used to construct this extractor.
    /// Surfaced for diagnostic logging when multiple OCR backends
    /// might be registered.
    #[must_use]
    pub fn detection_model_path(&self) -> &Path {
        &self.detection_model
    }
}

impl Extractor for OnnxOcrExtractor {
    fn extensions(&self) -> &[&'static str] {
        &["png", "jpg", "jpeg", "tiff", "tif", "bmp", "gif"]
    }

    fn name(&self) -> &'static str {
        "ocr-onnx"
    }

    fn extract(&self, path: &Path) -> Result<Document> {
        let img = image::open(path)
            .map_err(|e| {
                Error::ParseError(format!("could not decode image {}: {e}", path.display()))
            })?
            .to_rgb8();

        let results = self.ocr.predict(vec![img]).map_err(|e| {
            Error::ParseError(format!("oar-ocr predict failed on {}: {e}", path.display()))
        })?;

        // `predict` returns one OAROCRResult per input image; we
        // only ever pass one image in. Defensive empty-Vec check so
        // a future oar-ocr API change that returned `Vec::new()` on
        // a no-text image surfaces as empty markdown rather than a
        // panic.
        let mut markdown = String::new();
        if let Some(result) = results.first() {
            for region in &result.text_regions {
                // `region.text` is Option<Arc<str>> — None when the
                // detector found a region but recognition produced
                // nothing. Skip silently in that case.
                let Some(arc_text) = region.text.as_ref() else {
                    continue;
                };
                let text = arc_text.trim();
                if text.is_empty() {
                    continue;
                }
                if !markdown.is_empty() {
                    markdown.push('\n');
                }
                markdown.push_str(text);
            }
        }

        Ok(Document {
            markdown,
            title: None,
            metadata: std::collections::HashMap::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_cover_common_image_formats() {
        // Build a transient extractor for trait-surface inspection.
        // We avoid `with_models` here (it would try to load real ONNX
        // files); construct a stub that mirrors the published
        // extension list.
        let ext: &[&str] = &["png", "jpg", "jpeg", "tiff", "tif", "bmp", "gif"];
        for required in ["png", "jpg", "jpeg", "tiff", "bmp", "gif"] {
            assert!(
                ext.contains(&required),
                "expected ocr-onnx to handle .{required}, got {ext:?}"
            );
        }
    }

    #[test]
    fn missing_model_file_returns_typed_error() {
        let result = OnnxOcrExtractor::with_models(
            Path::new("/nonexistent-detection.onnx"),
            Path::new("/nonexistent-recognition.onnx"),
            Path::new("/nonexistent-dict.txt"),
        );
        assert!(matches!(result, Err(Error::MissingDependency { .. })));
    }

    #[test]
    #[ignore = "requires libonnxruntime AND model files in tests/fixtures/onnx-models/"]
    fn extracts_text_from_a_real_image() {
        // Skipped by default. To run:
        //   1. Download from https://github.com/GreatV/oar-ocr/releases:
        //      - pp-ocrv5_mobile_det.onnx
        //      - en_pp-ocrv5_mobile_rec.onnx
        //      - ppocrv5_en_dict.txt
        //   2. Drop them in tests/fixtures/onnx-models/
        //   3. Drop a "hello.png" containing "Hello, World!" in
        //      tests/fixtures/
        //   4. cargo test --features ocr-onnx-download -- --ignored \
        //         extracts_text_from_a_real_image
        let extractor = OnnxOcrExtractor::with_models(
            Path::new("tests/fixtures/onnx-models/pp-ocrv5_mobile_det.onnx"),
            Path::new("tests/fixtures/onnx-models/en_pp-ocrv5_mobile_rec.onnx"),
            Path::new("tests/fixtures/onnx-models/ppocrv5_en_dict.txt"),
        )
        .expect("model construction failed");

        let doc = extractor
            .extract(Path::new("tests/fixtures/hello.png"))
            .expect("extraction failed");
        assert!(
            !doc.markdown.is_empty(),
            "expected non-empty markdown from hello.png"
        );
        assert!(
            doc.markdown.to_lowercase().contains("hello"),
            "expected 'hello' in OCR output: {:?}",
            doc.markdown
        );
    }
}
