# mdkit

**A Rust toolkit for getting markdown out of any document.** Built for
Tauri / Iced / native desktop apps that want best-in-class document
extraction without a 350 MB Python sidecar.

> **Status:** v0.7 — **API stability candidate for 1.0**. Format
> coverage closed in v0.6 (PDF with scanned + mixed-content OCR,
> Pandoc-formats, spreadsheets, CSV/TSV, HTML, OCR backends for
> macOS / Windows / Linux). v0.7 freezes the public surface — see
> the [stability section](#stability-v07) below for what's locked
> in. v0.7.x will iterate on examples + cookbook docs. 1.0 ships
> once the API is exercised by at least one downstream production
> user. Watch / star the repo to follow along.

## Why this exists

Most Rust desktop apps that need to read DOCX / PDF / PPTX today have
two choices, both bad:

1. **Bundle a Python sidecar with [markitdown][markitdown]** — works
   well, ~350 MB on disk, ~1 second cold-start per parse, single
   process is GIL-locked. Fine for hobby projects, painful at scale.
2. **Use [markitdown-rs][markitdown-rs]** — pure Rust, much smaller,
   but PDF extraction is basic (no layout, no OCR) and DOCX support
   drops headings, lists, hyperlinks, images.

`mdkit` is the third option: **dispatch to the best tool per format,
prefer in-process Rust crates and OS-native APIs, fall back to a
single Pandoc binary for the formats Pandoc owns the gold standard
for.**

The composition we ship by default:

| Format | Backend | Why |
|---|---|---|
| DOCX, PPTX, EPUB, RTF, ODT, LaTeX | [Pandoc][pandoc] sidecar | Best-in-world conversion quality. ~150 MB but you bundle it once. |
| PDF (text) | [`pdfium-render`][pdfium] | Google's Pdfium engine, in-process, layout-aware. ~5 MB. |
| PDF (scanned + mixed-content) | Pdfium renders pages → platform OCR (Vision / Windows.Media.Ocr) | Per-page composition — pages with text pass through pdfium, empty pages get OCR'd. Auto-wired by `Engine::with_defaults` when both `pdf` and `ocr-platform` features are on. |
| Standalone images | Platform-native OCR — Vision.framework on macOS, Windows.Media.Ocr on Windows, ONNX-based ([Surya][surya]) on Linux | OS-quality on Mac/Win for free. ONNX models on Linux. |
| XLSX, XLS, ODS | [`calamine`][calamine] | Already the Rust ecosystem standard. |
| CSV, TSV | [`csv`][csv] | Stdlib-quality. |
| HTML | [`html2md`][html2md] (or Pandoc, configurable) | Default cheap, optional best. |

Total binary size with all backends: **~50-200 MB** depending on
which optional features you enable, vs ~350 MB for a Python
markitdown sidecar.

## Design principles

1. **Best output per format, not uniform mediocrity.** A single Rust
   crate that handles 20 formats poorly is worse than a dispatcher
   that uses Pandoc for what Pandoc is best at and Pdfium for what
   Pdfium is best at.
2. **OS-native first.** macOS PDFKit + Vision.framework, Windows
   Windows.Data.Pdf + Windows.Media.Ocr — these are battle-tested
   parsers Apple and Microsoft already paid for. We use them.
3. **In-process where possible, sidecar where necessary.** Process
   spawn is ~50-100 ms per file. For a folder of 1,000 files, that's
   real time. Pandoc is the one sidecar we accept; everything else is
   a Rust crate or OS-native FFI.
4. **Privacy-respecting.** Every extractor runs entirely on-device.
   No telemetry, no cloud round-trips, no analytics. (LLM-based image
   description is opt-in and uses the caller's own provider key.)
5. **Graceful degradation.** A bad PDF doesn't crash the process; it
   returns a typed error. Missing optional dependencies don't break
   the build; they disable specific extractors via feature flags.
6. **Small, stable, public surface.** The `Extractor` trait + `Engine`
   dispatcher are the API. Backends are implementation details that
   can be swapped without breaking callers.

## Quick start

```rust
use mdkit::Engine;
use std::path::Path;

let engine = Engine::with_defaults();
let doc = engine.extract(Path::new("report.pdf"))?;
println!("{}", doc.markdown);
```

To register your own extractor for a custom format:

```rust
use mdkit::{Engine, Extractor, Document, Result};
use std::path::Path;

struct MyParser;

impl Extractor for MyParser {
    fn extensions(&self) -> &[&'static str] { &["custom"] }
    fn extract(&self, path: &Path) -> Result<Document> {
        Ok(Document::new(std::fs::read_to_string(path)?))
    }
}

let mut engine = Engine::new();
engine.register(Box::new(MyParser));
```

## Feature flags

`mdkit` ships with backends behind feature flags so you only pay for
what you use:

```toml
[dependencies]
mdkit = { version = "0.1", features = ["pdf", "pandoc", "ocr-platform", "calamine"] }
```

| Feature | Adds | Approx. size cost |
|---|---|---|
| `pdf` | `pdfium-render` for PDF text extraction | ~5 MB |
| `pandoc` | Pandoc sidecar wrapper for DOCX/PPTX/EPUB/RTF/ODT/LaTeX | ~150 MB sidecar to bundle separately |
| `ocr-platform` | macOS Vision.framework + Windows.Media.Ocr (Linux falls back to `ocr-onnx`) | 0 on macOS/Win |
| `ocr-onnx` | ONNX-runtime PaddleOCR via [`oar-ocr`][oar-ocr] — works on all platforms incl. Linux. Caller supplies model files (~12 MB total) and `libonnxruntime`. | ~30 MB compiled |
| `ocr-onnx-download` | Adds `ocr-onnx` and lets `oar-ocr` fetch ONNX Runtime native libs at build / first use | 0 (downloads at build) |
| `calamine` | XLSX / XLS / ODS via `calamine` | ~1 MB |
| `csv` | CSV / TSV | <1 MB |
| `html` | HTML via `html2md` | <1 MB |
| `default` | `pdf`, `calamine`, `csv`, `html` (the in-process Rust ones) | ~7 MB |

Not enabling `pandoc` or `ocr-platform` is fine — extractors for those
formats simply won't be registered, and `Engine::extract` will return
`Error::UnsupportedFormat` for them.

## License

Dual-licensed under [MIT](LICENSE-MIT) OR [Apache 2.0](LICENSE-APACHE)
at your option. SPDX: `MIT OR Apache-2.0`.

## Stability (v0.7+) {#stability-v07}

v0.7 is the **API stability candidate** for 1.0. The following surface
is committed to and will only change with a major version bump:

- The `Extractor` trait shape — required methods, default implementations, `Send + Sync` bound.
- `Engine` construction + dispatch — `new`, `with_defaults`, `with_defaults_diagnostic`, `register`, `extract`, `extract_bytes`, `len`, `is_empty`.
- `Document` field set + `Document::new`. Marked `#[non_exhaustive]` so we can add fields (page count, language, confidence) without major bumps.
- `Error` enum semantics. Marked `#[non_exhaustive]` so we can add variants without major bumps. **Pattern-matchers must include a wildcard arm.**
- Feature flag names: `pdf`, `pandoc`, `calamine`, `csv`, `html`, `ocr-platform`, `ocr-onnx`, `ocr-onnx-download`, `full`.
- Backend `name()` strings — used by callers for filtering / logging.

The following are **implementation details** and may change in minor versions:

- Internal layout of any specific extractor (private fields, helper methods).
- Exact set of `Document.metadata` keys per backend (new keys may appear; documented keys stay).
- Auto-registration order in `Engine::with_defaults` (when multiple backends claim overlapping extensions; documented priority stays).
- Internal sidecar / FFI details (Pandoc's `--server` mode, ONNX runtime version, libpdfium binding).

1.0 will be cut once the API is exercised by at least one downstream production user.

## Status & roadmap

This is a young project. v0.1 ships the trait surface, dispatch
engine, and a no-op test extractor. Real backends land per the
roadmap below:

- [x] **v0.2 — `pdf` feature (`pdfium-render` integration).** `PdfiumExtractor`
      registers automatically in `Engine::with_defaults()`; falls back
      gracefully when libpdfium isn't installed. See `src/pdf.rs` for
      libpdfium installation notes.
- [x] **v0.3 — `pandoc` feature.** `PandocExtractor` covers DOCX, PPTX,
      EPUB, RTF, ODT, LaTeX, HTML via the `pandoc` binary. Auto-
      registers when `pandoc` is on PATH; supports
      `with_binary(absolute_path)` for shipping pandoc next to your
      app. CHANGELOG.md added.
- [x] **v0.4 — `calamine` + `csv` + `html` features (all in-process).**
      `CalamineExtractor` (XLSX/XLS/XLSB/XLSM/ODS), `CsvExtractor`
      (CSV/TSV with auto-delimiter), `Html2mdExtractor` (HTML/HTM,
      registered before Pandoc so it wins by default for HTML).
      Engine registration order documented inline.
- [x] **v0.5 — `ocr-platform` feature (macOS Vision + Windows.Media.Ocr).**
      `VisionOcrExtractor` (macOS) handles PNG / JPG / TIFF / BMP /
      GIF / HEIC via Apple Vision (neural-network-based, Apple Neural
      Engine-accelerated on Apple Silicon). `WindowsOcrExtractor`
      (Windows) handles PNG / JPG / TIFF / BMP / GIF via the OS-built-in
      Windows.Media.Ocr engine. v0.5.3 adds **scanned-PDF → OCR
      composition**: `PdfiumExtractor` accepts an OCR fallback at
      construction; when text extraction yields empty markdown, each
      page is rendered to a PNG and routed through OCR with `## Page N`
      headings. Linux ONNX backend lands in v0.6.
- [x] **v0.6 — `ocr-onnx` feature (PaddleOCR via [`oar-ocr`][oar-ocr]).**
      `OnnxOcrExtractor` wraps PaddleOCR's detection + recognition
      ONNX models through `ort`; works on Linux + macOS + Windows +
      WebAssembly. Caller supplies the three model files
      (det + rec + dict, ~12 MB total English-only) — download from
      <https://github.com/GreatV/oar-ocr/releases>. Opt into
      `ocr-onnx-download` to let oar-ocr fetch the ONNX Runtime
      native lib for you.
- [x] **v0.7 — API stability candidate (`#[non_exhaustive]` audit, `#[must_use]` audit, stability commitments doc).**
      v0.7.0 freezes the public surface for 1.0. v0.7.x will iterate
      on examples + cookbook docs + niche backend polish that doesn't
      change the public API. 1.0 ships once the API is exercised by
      at least one downstream production user.

Issues, PRs, and design discussion welcome at
<https://github.com/mdkit-project/mdkit/issues>.

## Used by

`mdkit` was extracted from the document-extraction layer of [Sery
Link][sery], a privacy-respecting data network for the files on your
machines. If you use `mdkit` in your project, please open a PR to
add yourself here.

## Acknowledgements

`mdkit` would not exist without:

- [markitdown][markitdown] — Microsoft's Python implementation, the
  prior art and quality benchmark for "any-doc-to-markdown."
- [markitdown-rs][markitdown-rs] — `uhobnil`'s Rust port, which
  proved the Rust-side feasibility and inspired the dispatch design.
- [Pandoc][pandoc] — John MacFarlane's universal document converter,
  the standard the academic publishing world is built on.
- [Pdfium][pdfium] — Google's PDF engine, free for everyone to use.
- [calamine][calamine] — `tafia`'s industry-standard Rust XLSX parser.

[markitdown]: https://github.com/microsoft/markitdown
[markitdown-rs]: https://github.com/uhobnil/markitdown-rs
[oar-ocr]: https://github.com/GreatV/oar-ocr
[pandoc]: https://pandoc.org
[pdfium]: https://pdfium.googlesource.com/pdfium/
[calamine]: https://github.com/tafia/calamine
[csv]: https://crates.io/crates/csv
[html2md]: https://crates.io/crates/html2md
[surya]: https://github.com/VikParuchuri/surya
[sery]: https://sery.ai
