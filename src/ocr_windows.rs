// Windows OCR backend uses raw FFI via the `windows` crate; many of
// the WinRT surface methods are still marked `unsafe` (notably
// `RoInitialize`). Allow it locally with a clear safety note rather
// than relaxing the crate-wide `deny(unsafe_code)` lint.
#![allow(unsafe_code)]

//! Windows OCR via the [Windows.Media.Ocr API](https://learn.microsoft.com/en-us/uwp/api/windows.media.ocr).
//!
//! Microsoft's built-in OCR ships with every Windows 10/11 install.
//! It's ML-based, supports the languages installed on the system as
//! "language packs with OCR support," and runs entirely on-device.
//! There is no runtime dependency to install — Windows.Media.Ocr is
//! part of the OS.
//!
//! ## What this extractor handles
//!
//! Standalone image files: PNG, JPG/JPEG, TIFF/TIF, BMP, GIF. (The
//! Windows imaging stack doesn't include HEIC/HEIF in the base OS,
//! so we omit those extensions on this backend even though the
//! macOS Vision backend handles them.)
//!
//! ## Threading
//!
//! Windows.Media.Ocr requires an MTA (multi-threaded apartment).
//! Calling [`extract`](WindowsOcrExtractor::extract) initialises the
//! current thread's apartment to MTA on first use; if the thread is
//! already in STA (typical for UI/main threads), extraction returns
//! a typed error suggesting the caller dispatch to a worker thread.
//! For Tauri apps, `tauri::async_runtime::spawn_blocking` is the
//! usual pattern.
//!
//! ## Image-size cap
//!
//! `OcrEngine::MaxImageDimension` is ~2600 px on shipping Windows.
//! Larger images currently return [`Error::ParseError`] with a clear
//! message — auto-downscale via `BitmapTransform` is planned for a
//! follow-up release. Most screenshots and scanned receipts fit
//! comfortably under the cap; high-DPI A4 page scans (e.g. 300 dpi
//! letter is 2550×3300) will not.
//!
//! ## Output shape
//!
//! Each `OcrLine` becomes one line of markdown in reading order.
//! Confidence scores and bounding boxes are not surfaced in
//! [`Document`](crate::Document) today — they're available in the
//! Windows OCR API but the [`Extractor`](crate::Extractor) trait
//! surface is intentionally simple.

use crate::{Document, Error, Extractor, Result};
use std::path::Path;
use windows::core::HSTRING;
use windows::Globalization::Language;
use windows::Graphics::Imaging::BitmapDecoder;
use windows::Media::Ocr::OcrEngine;
use windows::Storage::{FileAccessMode, StorageFile};

/// Windows.Media.Ocr-backed OCR extractor.
///
/// Construct via [`WindowsOcrExtractor::new`] (cannot fail —
/// Windows.Media.Ocr ships with the OS, no runtime check needed).
/// On non-Windows targets this type doesn't exist; the module is
/// gated by both the `ocr-platform` feature AND `target_os = "windows"`.
#[derive(Default)]
pub struct WindowsOcrExtractor;

impl WindowsOcrExtractor {
    /// Construct an extractor. Cannot fail — the Windows OCR engine
    /// ships with the OS. (Per-call init may still fail if no
    /// installed language pack supports OCR; see
    /// [`extract`](Self::extract).)
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Extractor for WindowsOcrExtractor {
    fn extensions(&self) -> &[&'static str] {
        &["png", "jpg", "jpeg", "tiff", "tif", "bmp", "gif"]
    }

    fn name(&self) -> &'static str {
        "ocr-windows"
    }

    fn extract(&self, path: &Path) -> Result<Document> {
        ensure_mta_initialized()?;
        extract_with_windows_ocr(path)
    }
}

/// Initialise the current thread's COM apartment as MTA. Idempotent
/// across calls on the same thread (returns Ok if already MTA).
/// Returns an explanatory error if the thread is locked into STA — we
/// can't switch modes, so the caller has to dispatch to a worker.
fn ensure_mta_initialized() -> Result<()> {
    use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};

    // HRESULT codes we care about distinguishing from "real" errors:
    //   S_FALSE (0x00000001)         — already initialised in MTA, fine.
    //   RPC_E_CHANGED_MODE (0x80010106) — thread is STA, can't change.
    const S_FALSE: i32 = 1;
    const RPC_E_CHANGED_MODE: i32 = 0x8001_0106u32 as i32;

    let result = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
    match result {
        Ok(()) => Ok(()),
        Err(e) if e.code().0 == S_FALSE => Ok(()),
        Err(e) if e.code().0 == RPC_E_CHANGED_MODE => Err(Error::ParseError(
            "Windows OCR needs an MTA thread; the calling thread is in STA mode \
             (typically a UI/main thread). Dispatch to a worker thread \
             (e.g. tokio::task::spawn_blocking, std::thread::spawn, or Tauri's \
             tauri::async_runtime::spawn_blocking) and call extract() from there."
                .into(),
        )),
        Err(e) => Err(Error::ParseError(format!(
            "RoInitialize(MTA) failed: {e:?}"
        ))),
    }
}

fn extract_with_windows_ocr(path: &Path) -> Result<Document> {
    // 1. Canonicalise to an absolute path. Windows.Media.Ocr's
    //    `StorageFile::GetFileFromPathAsync` only accepts absolute
    //    paths with backslashes — `canonicalize()` on Windows
    //    produces exactly that.
    let absolute_path = path.canonicalize().map_err(|e| {
        Error::ParseError(format!("could not canonicalize {}: {e}", path.display()))
    })?;
    let absolute_str = absolute_path.to_str().ok_or_else(|| {
        Error::ParseError(format!(
            "canonical path is not valid UTF-8: {}",
            absolute_path.display()
        ))
    })?;
    let path_h = HSTRING::from(absolute_str);

    // 2. StorageFile → IRandomAccessStream → BitmapDecoder →
    //    SoftwareBitmap. Each `*Async()` returns an
    //    `IAsyncOperation<T>`; `.get()` blocks the current (MTA)
    //    thread until completion.
    let file = StorageFile::GetFileFromPathAsync(&path_h)
        .map_err(|e| Error::ParseError(format!("GetFileFromPathAsync failed: {e:?}")))?
        .get()
        .map_err(|e| Error::ParseError(format!("StorageFile open await failed: {e:?}")))?;

    let stream = file
        .OpenAsync(FileAccessMode::Read)
        .map_err(|e| Error::ParseError(format!("StorageFile::OpenAsync failed: {e:?}")))?
        .get()
        .map_err(|e| Error::ParseError(format!("stream open await failed: {e:?}")))?;

    let decoder = BitmapDecoder::CreateAsync(&stream)
        .map_err(|e| Error::ParseError(format!("BitmapDecoder::CreateAsync failed: {e:?}")))?
        .get()
        .map_err(|e| Error::ParseError(format!("BitmapDecoder await failed: {e:?}")))?;

    let bitmap = decoder
        .GetSoftwareBitmapAsync()
        .map_err(|e| Error::ParseError(format!("GetSoftwareBitmapAsync failed: {e:?}")))?
        .get()
        .map_err(|e| Error::ParseError(format!("SoftwareBitmap await failed: {e:?}")))?;

    // 3. Build an OcrEngine. Prefer the user's installed profile
    //    languages so a German user gets German OCR; fall back to
    //    en-US so we still produce something useful when none of the
    //    user-profile languages have OCR packs installed.
    let engine = match OcrEngine::TryCreateFromUserProfileLanguages() {
        Ok(e) => e,
        Err(_) => {
            let en = Language::CreateLanguage(&HSTRING::from("en-US")).map_err(|e| {
                Error::ParseError(format!("Language::CreateLanguage(en-US) failed: {e:?}"))
            })?;
            OcrEngine::TryCreateFromLanguage(&en).map_err(|e| {
                Error::ParseError(format!(
                    "Windows OCR engine init failed — no installed language pack \
                     supports OCR. Install one via Settings → Time & Language → \
                     Language → Add a language → Optional features → OCR. \
                     Underlying error: {e:?}"
                ))
            })?
        }
    };

    // 4. Bounds-check against `MaxImageDimension`. Windows returns a
    //    deep WinRT error if the image exceeds it; we'd rather give
    //    callers a typed, descriptive error up front.
    let max_dim_u = OcrEngine::MaxImageDimension()
        .map_err(|e| Error::ParseError(format!("MaxImageDimension query failed: {e:?}")))?;
    let max_dim = i32::try_from(max_dim_u).unwrap_or(i32::MAX);
    let w = bitmap
        .PixelWidth()
        .map_err(|e| Error::ParseError(format!("SoftwareBitmap::PixelWidth failed: {e:?}")))?;
    let h = bitmap
        .PixelHeight()
        .map_err(|e| Error::ParseError(format!("SoftwareBitmap::PixelHeight failed: {e:?}")))?;
    if w > max_dim || h > max_dim {
        return Err(Error::ParseError(format!(
            "image is {w}x{h}, exceeds Windows OCR max dimension of {max_dim}px. \
             Downscale before passing in. (Auto-downscale is planned for a future release.)"
        )));
    }

    // 5. Recognise. RecognizeAsync returns an IAsyncOperation<OcrResult>;
    //    OcrResult::Lines() yields an IVectorView<OcrLine>; each
    //    OcrLine carries the recognised text plus per-word bounding
    //    boxes (we surface only the text for now).
    let result = engine
        .RecognizeAsync(&bitmap)
        .map_err(|e| Error::ParseError(format!("OcrEngine::RecognizeAsync failed: {e:?}")))?
        .get()
        .map_err(|e| Error::ParseError(format!("OCR result await failed: {e:?}")))?;

    let lines = result
        .Lines()
        .map_err(|e| Error::ParseError(format!("OcrResult::Lines failed: {e:?}")))?;

    let mut markdown = String::new();
    for line in lines {
        let text = line.Text().map(|h| h.to_string()).unwrap_or_default();
        if !text.trim().is_empty() {
            if !markdown.is_empty() {
                markdown.push('\n');
            }
            markdown.push_str(&text);
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
        let ext = WindowsOcrExtractor.extensions();
        for required in ["png", "jpg", "jpeg", "tiff", "bmp", "gif"] {
            assert!(
                ext.contains(&required),
                "expected ocr-windows to handle .{required}, got {ext:?}"
            );
        }
    }

    #[test]
    fn name_identifies_backend() {
        assert_eq!(WindowsOcrExtractor.name(), "ocr-windows");
    }

    #[test]
    #[ignore = "requires a real image file with text in tests/fixtures/ on a Windows host"]
    fn extracts_text_from_a_real_image() {
        // Skipped by default. Run with:
        //   cargo test --features ocr-platform -- --ignored
        // on a Windows host, after dropping a "hello.png" containing
        // the literal text "Hello, World!" into tests/fixtures/.
        let extractor = WindowsOcrExtractor::new();
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
            WindowsOcrExtractor.extract(std::path::Path::new("C:\\nonexistent-image-here.png"));
        // Either canonicalize fails (most common) or RoInitialize fails
        // on this thread — both surface as ParseError.
        assert!(matches!(result, Err(Error::ParseError(_))));
    }
}
