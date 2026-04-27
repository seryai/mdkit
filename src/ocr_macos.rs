// Inner attributes (allow / cfg) come before doc comments to avoid
// splitting the module-level //! block. `unsafe_code` is allowed here
// because Vision FFI requires it for some calls (raw-pointer args to
// CGImageForProposedRect_context_hints); other ObjC calls in objc2
// v0.6 are now safe and don't need unsafe blocks.
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
use objc2_app_kit::NSImage;
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
/// `autoreleasepool` so the autoreleased Cocoa objects (`NSImage`,
/// `CGImageRefs`) get cleaned up promptly rather than living until
/// the Tauri runloop drains its pool.
fn extract_with_vision(path: &Path) -> Result<Document> {
    // 1. Load the image file as NSImage. NSImage understands every
    //    format Image I/O supports (which includes everything in
    //    extensions() above).
    let path_str = path
        .to_str()
        .ok_or_else(|| Error::ParseError(format!("path is not valid UTF-8: {}", path.display())))?;

    let url = NSURL::fileURLWithPath(&NSString::from_str(path_str));
    let nsimage = NSImage::initWithContentsOfURL(NSImage::alloc(), &url).ok_or_else(|| {
        Error::ParseError(format!(
            "could not load image (unsupported format or corrupt): {path_str}"
        ))
    })?;

    // 2. Convert NSImage to CGImage. Vision works on CGImage, not
    //    NSImage. The proposedRect+context+hints arguments are the
    //    "give me the image at the proposed size with default
    //    rendering" path.
    let cg_image =
        unsafe { nsimage.CGImageForProposedRect_context_hints(std::ptr::null_mut(), None, None) }
            .ok_or_else(|| {
            Error::ParseError(format!("NSImage→CGImage conversion failed: {path_str}"))
        })?;

    // 3. Build the recognizer request. Default to "accurate"
    //    recognition level (slower than fast, but the quality
    //    difference is significant — and we're already paying the
    //    process-startup cost).
    let request = {
        let req = VNRecognizeTextRequest::new();
        req.setRecognitionLevel(objc2_vision::VNRequestTextRecognitionLevel::Accurate);
        req.setUsesLanguageCorrection(true);
        req
    };

    // 4. Hand the image to a request handler and run synchronously.
    //    perform_requests blocks; that's fine — extraction is CPU-
    //    bound and the caller decides whether to off-load to a
    //    thread pool (Tauri's blocking task spawner is the usual
    //    pattern).
    let handler = unsafe {
        let options = objc2_foundation::NSDictionary::<NSString, objc2::runtime::AnyObject>::new();
        VNImageRequestHandler::initWithCGImage_options(
            VNImageRequestHandler::alloc(),
            &cg_image,
            &options,
        )
    };

    // VNRecognizeTextRequest → VNImageBasedRequest → VNRequest.
    // performRequests_error wants &NSArray<VNRequest>, so we double-upcast.
    let request_as_vnrequest: Retained<VNRequest> = request.clone().into_super().into_super();
    let requests: Retained<NSArray<VNRequest>> =
        NSArray::from_retained_slice(&[request_as_vnrequest]);

    handler
        .performRequests_error(&requests)
        .map_err(|e| Error::ParseError(format!("Vision performRequests failed: {e:?}")))?;

    // 5. Collect observations. Vision returns
    //    `[VNRecognizedTextObservation]` in `request.results`.
    //    Each observation carries one or more candidate strings
    //    (we take the top one) plus a bounding box.
    let observations = request.results().unwrap_or_else(NSArray::new);

    let mut markdown = String::new();
    for obs in &observations {
        // Each result is a VNObservation; downcast to
        // VNRecognizedTextObservation to access topCandidates.
        let text_obs = obs.downcast_ref::<VNRecognizedTextObservation>();
        let Some(text_obs) = text_obs else { continue };

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
