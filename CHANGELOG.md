# Changelog

All notable changes to mdkit are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and mdkit
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

mdkit is pre-1.0 — the public API surface (`Extractor`, `Engine`,
`Document`, `Error`) is intended to stay stable, but minor versions
may introduce additive changes to backends, feature flags, and
auxiliary types until 1.0 lands.

## [Unreleased]

## [0.5.1] — 2026-04-27

### Fixed

- **`VisionOcrExtractor` returned empty markdown for valid images.**
  v0.5.0 loaded each image via `NSImage::initWithContentsOfURL`, then
  rasterized to a `CGImage` through
  `CGImageForProposedRect_context_hints`, then handed the `CGImage` to
  `VNImageRequestHandler::initWithCGImage_options`. The pipeline ran
  without error, but Vision found zero text observations on every
  input — the multi-step conversion was silently producing a CGImage
  Vision couldn't read. v0.5.1 switches to
  `VNImageRequestHandler::initWithURL_options`, which lets Vision
  load the file directly via Image I/O. Confirmed end-to-end on a
  PNG: Vision now returns the expected text at confidence 1.0.

### Changed

- Dropped the NSImage / CGImage extraction step from
  `src/ocr_macos.rs`. The `objc2-app-kit` and `objc2-core-graphics`
  crates are still pulled in by the `ocr-platform` feature for
  forward compatibility, but the OCR backend itself no longer touches
  either — the `VNImageRequestHandler::initWithURL_options` path goes
  straight from filesystem URL to Vision request.

## [0.5.0] — 2026-04-27

### Added

- **`VisionOcrExtractor`** — macOS OCR via Apple's Vision framework
  (`VNRecognizeTextRequest`). Neural-network-based, accelerated on
  the Apple Neural Engine on Apple Silicon, handles handwriting and
  mixed languages well, and ships free with every macOS install.
  Gated by the `ocr-platform` feature; only present on macOS targets
  (Windows + Linux are no-ops in v0.5; Windows lands in v0.5.x via
  `Windows.Media.Ocr`, Linux in v0.6 via `ocr-onnx`).
- Handles standalone image files: PNG, JPG/JPEG, TIFF/TIF, BMP, GIF,
  HEIC/HEIF.
- Auto-registration in `Engine::with_defaults` when both the
  `ocr-platform` feature is enabled and the target is macOS.
- Output is one line of markdown per Vision text observation, in
  reading order. Confidence scores and bounding boxes are not
  surfaced today (the `Extractor` trait surface stays simple); a
  future "rich extraction" trait could expose them.
- Runs inside an `autoreleasepool` so autoreleased Cocoa objects get
  cleaned up promptly.

### Changed

- `[lints.rust] unsafe_code` downgraded from `forbid` to `deny`.
  Backends with legitimate FFI needs (the macOS Vision module is
  the first) can now opt in via per-module `#![allow(unsafe_code)]`
  with a clear safety comment. Core dispatch and trait-only
  backends remain unsafe-free.
- Scanned-PDF OCR is **not** wired in v0.5 — `PdfiumExtractor`
  still returns empty markdown for image-only PDFs. A future
  release will detect the empty-result case and route through OCR
  automatically.

### Notes

- `objc2-vision` is already partially safe in v0.6 — most calls to
  Vision APIs don't require `unsafe`. The remaining `unsafe` block
  is for `CGImageForProposedRect_context_hints`, which takes raw
  `*mut NSRect` for the optional out-rect parameter.
- `objc2-vision`, `objc2-app-kit`, `objc2-foundation`, and
  `objc2-core-graphics` are pulled in only on macOS via a target-
  specific dependency block, gated additionally by the
  `ocr-platform` feature. Builds on Windows/Linux with
  `--features ocr-platform` succeed but register no OCR extractor.

## [0.4.0] — 2026-04-27

### Added

- **`CalamineExtractor`** — XLSX, XLS, XLSB, XLSM, ODS spreadsheet
  extraction via the [`calamine`](https://crates.io/crates/calamine)
  crate (gated by the `calamine` feature). Each worksheet renders as
  a markdown table preceded by an `## ` heading with the sheet name;
  ragged rows pad/truncate to the header column count to keep the
  table well-formed.
- **`CsvExtractor`** — CSV and TSV extraction via the
  [`csv`](https://crates.io/crates/csv) crate (gated by the `csv`
  feature). Auto-selects tab delimiter for `.tsv` files. First row
  treated as the header row; data rendered as a markdown table.
- **`Html2mdExtractor`** — HTML and HTM extraction via the
  [`html2md`](https://crates.io/crates/html2md) crate (gated by the
  `html` feature). Lighter and faster than the Pandoc HTML reader;
  registered before Pandoc in `Engine::with_defaults` so it wins for
  HTML files when both features are enabled.
- All three extractors implement the new pattern: `Default + new()`
  infallible constructors (no runtime dependency to verify), so they
  register unconditionally in `Engine::with_defaults` when their
  feature flag is on.

### Changed

- **Backend registration order in `Engine::with_defaults`** — cheap
  in-process Rust backends (PDF, calamine, csv, html2md) register
  before the Pandoc sidecar, so when format coverage overlaps (HTML
  is the only one today, but future formats may too), the in-process
  backend wins. Documented inline in `src/lib.rs`.
- README + roadmap reflect v0.4 ship.

### Notes

- Pipe characters (`|`) in spreadsheet/CSV cell values are escaped to
  `&#124;` to keep markdown tables well-formed; embedded newlines
  collapse to a single space for the same reason.
- The `csv` crate is referenced via `::csv::` in `src/csv.rs` to
  disambiguate from the local module of the same name. Module name
  matches the feature name for consistency with the other backends.

## [0.3.0] — 2026-04-27

### Added

- **`PandocExtractor`** for DOCX, PPTX, EPUB, RTF, ODT, LaTeX (`tex`,
  `latex`), and HTML (`html`, `htm`). Spawns the `pandoc` binary per
  file with a stdin/stdout protocol; outputs GitHub-Flavored Markdown
  (`gfm`). Gated by the `pandoc` feature.
- **`PandocExtractor::new`** locates `pandoc` on the system PATH and
  verifies it responds to `--version` before declaring success.
- **`PandocExtractor::with_binary`** uses an explicit binary path —
  preferred when shipping a static `pandoc` binary alongside your
  application (Tauri / Iced / similar).
- **`PandocExtractor::pandoc_from`** (associated function) maps file
  extensions to Pandoc reader names; exposed publicly so callers can
  pre-check whether a given file is supported.
- Auto-registration in `Engine::with_defaults()` when the `pandoc`
  feature is enabled. Falls back gracefully (logs via
  `with_defaults_diagnostic`, silently skips otherwise) when the
  pandoc binary isn't found.
- `CHANGELOG.md` (this file) — retroactive entries for v0.1.0 and
  v0.2.0 included for completeness.

### Changed

- README roadmap reflects v0.3 ship.

### Notes

- No persistent server mode yet (each `extract` call spawns a fresh
  `pandoc` process — ~50ms cold-start). Pandoc's `--server` mode
  amortizes that across calls; will land as an opt-in optimization
  in a later release.
- No PDF input via Pandoc by design — Pandoc deliberately doesn't
  read PDFs; mdkit's `Engine` dispatches PDF to the `pdf` backend
  (`PdfiumExtractor`) automatically when both features are enabled.

## [0.2.0] — 2026-04-27

### Added

- **`PdfiumExtractor`** for PDF text extraction via Google's Pdfium
  engine through the `pdfium-render` crate (gated by the `pdf`
  feature). In-process, layout-aware, no sidecar.
- **`PdfiumExtractor::new`** binds to libpdfium on the system library
  path; **`PdfiumExtractor::with_library_path`** binds from an
  explicit directory (Tauri-style "ship libpdfium next to the
  binary"). Both return `Error::MissingDependency` when libpdfium
  isn't found.
- **`Engine::with_defaults_diagnostic`** — new method alongside
  `Engine::with_defaults` that returns the engine plus a list of
  `(backend_name, Error)` for each backend that failed to register.
  Lets callers log "PDF support disabled: libpdfium not found"
  rather than silently shipping a degraded experience.
- Auto-registration of the PDF extractor in `Engine::with_defaults()`
  when libpdfium is available; engine still constructs successfully
  when libpdfium is missing (the PDF extractor is just absent).

### Changed

- `pdfium-render` dependency configured with `default-features =
  false` and only the `thread_safe` + `pdfium_latest` features —
  avoids pulling in the `image` crate weight since mdkit doesn't
  render PDF pages, only extracts text.

## [0.1.0] — 2026-04-27

### Added

- Initial release. Establishes the crate name on crates.io and the
  public API surface that backends will target.
- **`Engine`** — the dispatcher. Routes `extract(path)` calls to the
  registered `Extractor` for the file's extension.
- **`Extractor` trait** — the per-format integration point.
  Implementors declare `extensions()`, `name()`, `extract(path)`,
  and optionally `extract_bytes(bytes, ext)`.
- **`Document`** — the unit of output. `markdown` is always present;
  `title` and `metadata` are best-effort and may be empty.
- **Typed `Error` enum** — `Io`, `UnsupportedFormat`,
  `UnsupportedOperation`, `ParseError`, `MissingDependency`,
  `SidecarFailure`, `Other`. Coarse-grained on purpose; backends
  distinguished via `Extractor::name` when needed.
- **Feature flags pre-declared** (no-op placeholders so the public
  feature surface is stable from v0.1): `pdf`, `pandoc`,
  `ocr-platform`, `ocr-onnx`, `calamine`, `csv`, `html`, `full`,
  `default = ["pdf", "calamine", "csv", "html"]`.
- Dual-licensed under MIT OR Apache-2.0 (Rust ecosystem convention).
- CI workflow on Ubuntu + macOS + Windows (stable Rust + MSRV 1.75
  + clippy + rustfmt + cargo-audit gates).
- `CONTRIBUTING.md`, `SECURITY.md` for repo hygiene.

[Unreleased]: https://github.com/mdkit-project/mdkit/compare/v0.5.1...HEAD
[0.5.1]: https://github.com/mdkit-project/mdkit/compare/v0.5.0...v0.5.1
[0.5.0]: https://github.com/mdkit-project/mdkit/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/mdkit-project/mdkit/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/mdkit-project/mdkit/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/mdkit-project/mdkit/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/mdkit-project/mdkit/releases/tag/v0.1.0
