// Inner attributes (allow / cfg) come before doc comments to avoid
// splitting the module-level //! block. `unsafe_code` is allowed here
// because the Vision FFI call to `VNImageRequestHandler::
// initWithURL_options` is still marked unsafe in objc2-vision 0.3
// (alloc-init pattern); the rest of the Vision calls in objc2 0.6
// are safe and don't need unsafe blocks.
#![allow(unsafe_code)]

//! macOS OCR via the [Vision framework](https://developer.apple.com/documentation/vision)
//! (`VNRecognizeTextRequest`).
//!
//! Apple's Vision text recognizer is one of the best general-purpose
//! OCR engines available — neural-network-based, accelerated on the
//! Apple Neural Engine on Apple Silicon, handles handwriting and
//! mixed languages well, and ships free with every macOS install.
//! There is no runtime dependency to install — Vision is part of
//! the OS.
//!
//! ## What this extractor handles
//!
//! Standalone image files: PNG, JPG/JPEG, TIFF/TIF, BMP, GIF, HEIC/HEIF.
//! Scanned-PDF OCR is NOT in this module — the
//! [`pdf`](crate::pdf) backend handles PDF; a future feature will
//! detect "PDF returned empty text" and route through OCR
//! automatically.
//!
//! ## Output shape
//!
//! Each Vision text observation becomes one line of markdown in
//! reading order. Confidence scores and bounding boxes are not
//! surfaced in [`Document`](crate::Document) today — they're
//! available in the Vision API but the [`Extractor`](crate::Extractor)
//! trait surface is intentionally simple. A future "rich extraction"
//! trait could expose them; for now we optimize for "text suitable
//! for AI grounding and full-text indexing."

use crate::{Document, Error, Extractor, Result};
use objc2::rc::{autoreleasepool, Retained};
use objc2::AnyThread;
use objc2_foundation::{NSArray, NSString, NSURL};
use objc2_vision::{
    VNImageRequestHandler, VNRecognizeTextRequest, VNRecognizedTextObservation, VNRequest,
};
use std::path::Path;

/// macOS Vision-backed OCR extractor.
///
/// Construct via [`VisionOcrExtractor::new`] (cannot fail — Vision
/// ships with macOS, no runtime check needed). On non-macOS targets
/// this type doesn't exist; the module is gated by both the
/// `ocr-platform` feature AND `target_os = "macos"`.
#[derive(Default)]
pub struct VisionOcrExtractor;

impl VisionOcrExtractor {
    /// Construct an extractor. Cannot fail — Vision is part of macOS.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Extractor for VisionOcrExtractor {
    fn extensions(&self) -> &[&'static str] {
        &[
            "png", "jpg", "jpeg", "tiff", "tif", "bmp", "gif", "heic", "heif",
        ]
    }

    fn name(&self) -> &'static str {
        "vision-macos"
    }

    fn extract(&self, path: &Path) -> Result<Document> {
        autoreleasepool(|_| extract_with_vision(path))
    }
}

/// Run the actual Vision OCR pipeline. Wrapped in an
/// `autoreleasepool` so autoreleased Cocoa objects (the `NSURL`,
/// `NSDictionary`, `VNImageRequestHandler`, and the result
/// `VNRecognizedTextObservation`s) get cleaned up promptly rather
/// than living until the Tauri runloop drains its pool.
fn extract_with_vision(path: &Path) -> Result<Document> {
    // 1. Build an NSURL from the canonical file path. Vision's
    //    `initWithURL_options` loads the image directly via Image I/O
    //    — much simpler than going NSImage → CGImage, and eliminates
    //    a whole class of "the conversion silently produced something
    //    Vision can't read" bugs that bit us in v0.5.0.
    let path_str = path
        .to_str()
        .ok_or_else(|| Error::ParseError(format!("path is not valid UTF-8: {}", path.display())))?;
    let absolute_path = path
        .canonicalize()
        .map_err(|e| Error::ParseError(format!("could not canonicalize path {path_str}: {e}")))?;
    let absolute_str = absolute_path.to_str().ok_or_else(|| {
        Error::ParseError(format!(
            "canonical path is not valid UTF-8: {}",
            absolute_path.display()
        ))
    })?;
    let url = NSURL::fileURLWithPath(&NSString::from_str(absolute_str));

    // 2. Build the recognizer request. "Accurate" is slower than
    //    "fast" but the quality difference is significant — and
    //    we're already paying the process-startup cost. Explicit
    //    recognition language so we don't depend on the system
    //    default (avoids Vision returning nothing on a non-English
    //    system).
    let request = {
        let req = VNRecognizeTextRequest::new();
        req.setRecognitionLevel(objc2_vision::VNRequestTextRecognitionLevel::Accurate);
        req.setUsesLanguageCorrection(true);
        let langs: Retained<NSArray<NSString>> =
            NSArray::from_retained_slice(&[NSString::from_str("en-US")]);
        req.setRecognitionLanguages(&langs);
        req
    };

    // 3. Hand the URL to a request handler and run synchronously.
    //    `performRequests_error` blocks; that's fine — extraction
    //    is CPU-bound and the caller decides whether to off-load to
    //    a thread pool (Tauri's blocking task spawner is the usual
    //    pattern).
    let handler = unsafe {
        let options = objc2_foundation::NSDictionary::<NSString, objc2::runtime::AnyObject>::new();
        VNImageRequestHandler::initWithURL_options(VNImageRequestHandler::alloc(), &url, &options)
    };

    // VNRecognizeTextRequest → VNImageBasedRequest → VNRequest.
    // performRequests_error wants &NSArray<VNRequest>, so we double-upcast.
    let request_as_vnrequest: Retained<VNRequest> = request.clone().into_super().into_super();
    let requests: Retained<NSArray<VNRequest>> =
        NSArray::from_retained_slice(&[request_as_vnrequest]);

    handler
        .performRequests_error(&requests)
        .map_err(|e| Error::ParseError(format!("Vision performRequests failed: {e:?}")))?;

    // 4. Collect observations. Vision returns
    //    `[VNRecognizedTextObservation]` in `request.results`.
    //    Each observation carries one or more candidate strings
    //    (we take the top one) plus a bounding box.
    let observations = request.results().unwrap_or_else(NSArray::new);

    let mut markdown = String::new();
    for obs in &observations {
        let Some(text_obs) = obs.downcast_ref::<VNRecognizedTextObservation>() else {
            continue;
        };
        let candidates = text_obs.topCandidates(1);
        let Some(top) = candidates.iter().next() else {
            continue;
        };
        let line: String = top.string().to_string();
        if !line.trim().is_empty() {
            if !markdown.is_empty() {
                markdown.push('\n');
            }
            markdown.push_str(&line);
        }
    }

    Ok(Document {
        markdown,
        title: None,
        metadata: std::collections::HashMap::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_cover_common_image_formats() {
        let ext = VisionOcrExtractor.extensions();
        for required in ["png", "jpg", "jpeg", "tiff", "heic"] {
            assert!(
                ext.contains(&required),
                "expected vision-macos to handle .{required}, got {ext:?}"
            );
        }
    }

    #[test]
    fn name_identifies_backend() {
        assert_eq!(VisionOcrExtractor.name(), "vision-macos");
    }

    #[test]
    #[ignore = "requires a real image file with text in tests/fixtures/"]
    fn extracts_text_from_a_real_image() {
        // Skipped by default. Run with:
        //   cargo test --features ocr-platform -- --ignored
        // after dropping a "hello.png" containing the literal text
        // "Hello, World!" into tests/fixtures/.
        let extractor = VisionOcrExtractor::new();
        let doc = extractor
            .extract(std::path::Path::new("tests/fixtures/hello.png"))
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

    #[test]
    fn missing_file_returns_typed_error() {
        let result =
            VisionOcrExtractor.extract(std::path::Path::new("/nonexistent-image-here.png"));
        assert!(matches!(result, Err(Error::ParseError(_))));
    }
}
