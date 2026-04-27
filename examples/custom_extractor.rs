//! Show how to implement the `Extractor` trait for a custom file
//! format — in this case, a deliberately silly "ROT13 text"
//! extractor that decodes `.rot` files (text where every letter
//! has been rotated 13 places). The point is the registration
//! pattern, not the format itself.
//!
//! ```bash
//! cargo run --example custom_extractor -- /path/to/file.rot
//! ```
//!
//! The mechanics that ARE realistic:
//!
//! 1. Implement `Extractor::extensions` to claim file extensions.
//! 2. Implement `Extractor::extract` to read the path and produce
//!    a `Document`.
//! 3. Optionally implement `Extractor::extract_bytes` if your
//!    format can read from a byte slice (this example does — most
//!    real formats can).
//! 4. Register the extractor on a fresh or default-built `Engine`.
//!    Calling `Engine::register` BEFORE the default registration
//!    chain wins for overlapping extensions; calling AFTER wins
//!    only when nothing else claims the extension.

use std::env;
use std::path::Path;
use std::process::ExitCode;

use mdkit::{Document, Engine, Error, Extractor, Result};

/// Decodes ROT13-rotated ASCII letters; passes everything else
/// through unchanged. Real-world equivalents would parse a
/// proprietary file format, decompress, transcode an audio
/// transcript, etc. — the trait shape stays the same.
struct Rot13Extractor;

impl Extractor for Rot13Extractor {
    fn extensions(&self) -> &[&'static str] {
        &["rot", "rot13"]
    }

    fn name(&self) -> &'static str {
        "rot13-example"
    }

    fn extract(&self, path: &Path) -> Result<Document> {
        let bytes = std::fs::read(path)?;
        // Delegate to extract_bytes so the two paths share the
        // decoding logic. `Error::Io` from std::fs::read converts
        // automatically via `?` thanks to `Error: From<io::Error>`.
        self.extract_bytes(&bytes, "rot")
    }

    fn extract_bytes(&self, bytes: &[u8], _ext: &str) -> Result<Document> {
        // ROT13 only meaningful for text. Reject non-UTF-8 input
        // with a typed error rather than silently mangling bytes.
        let text = std::str::from_utf8(bytes)
            .map_err(|e| Error::ParseError(format!("rot13: not valid UTF-8: {e}")))?;
        Ok(Document::new(rot13(text)))
    }
}

fn rot13(input: &str) -> String {
    input
        .chars()
        .map(|c| match c {
            'a'..='m' | 'A'..='M' => (c as u8 + 13) as char,
            'n'..='z' | 'N'..='Z' => (c as u8 - 13) as char,
            _ => c,
        })
        .collect()
}

fn main() -> ExitCode {
    let Some(path) = env::args().nth(1) else {
        eprintln!("usage: custom_extractor <path-to-.rot-file>");
        return ExitCode::FAILURE;
    };

    // Register the custom extractor BEFORE adding default backends.
    // Order matters: the first registered extractor that claims a
    // given extension wins on dispatch, so registering first lets
    // us override defaults.
    let mut engine = Engine::new();
    engine.register(Box::new(Rot13Extractor));

    // (No real defaults to chain in here — we'd register
    // `mdkit::pdf::PdfiumExtractor::new()?` etc. if we wanted
    // them too.)

    match engine.extract(Path::new(&path)) {
        Ok(doc) => {
            print!("{}", doc.markdown);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
