# Changelog

All notable changes to mdkit are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and mdkit
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

mdkit is pre-1.0 — the public API surface (`Extractor`, `Engine`,
`Document`, `Error`) is intended to stay stable, but minor versions
may introduce additive changes to backends, feature flags, and
auxiliary types until 1.0 lands.

## [Unreleased]

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

[Unreleased]: https://github.com/mdkit-project/mdkit/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/mdkit-project/mdkit/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/mdkit-project/mdkit/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/mdkit-project/mdkit/releases/tag/v0.1.0
