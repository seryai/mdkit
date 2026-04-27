//! Batch-extract every supported document in a folder, writing
//! one `.md` file per source into a sibling output folder.
//!
//! ```bash
//! cargo run --example batch -- /path/to/folder /path/to/output
//! cargo run --example batch --features "pandoc ocr-platform" -- /docs /docs-md
//! ```
//!
//! Output filenames mirror the source layout: `report.docx` →
//! `report.docx.md` (extension preserved + `.md` appended, so
//! collisions across same-stem-different-extension don't
//! overwrite). Source folders are walked non-recursively for
//! simplicity; pair with [`scankit`](https://crates.io/crates/scankit)
//! for recursive walks with exclude-glob support.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use mdkit::Engine;

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    let Some(input) = args.next() else {
        return usage();
    };
    let Some(output) = args.next() else {
        return usage();
    };

    let in_dir = PathBuf::from(input);
    let out_dir = PathBuf::from(output);
    if !in_dir.is_dir() {
        eprintln!("error: {} is not a directory", in_dir.display());
        return ExitCode::FAILURE;
    }
    if let Err(e) = fs::create_dir_all(&out_dir) {
        eprintln!("error: could not create {}: {e}", out_dir.display());
        return ExitCode::FAILURE;
    }

    let (engine, errors) = Engine::with_defaults_diagnostic();
    for (backend, err) in &errors {
        eprintln!("[batch] mdkit: backend `{backend}` not registered: {err}");
    }
    eprintln!();

    let entries = match fs::read_dir(&in_dir) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("error: read_dir {}: {e}", in_dir.display());
            return ExitCode::FAILURE;
        }
    };

    let mut success = 0u32;
    let mut skipped = 0u32;
    let mut failed = 0u32;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        match engine.extract(&path) {
            Ok(doc) => {
                let out_path = out_dir.join(format!(
                    "{}.md",
                    path.file_name().unwrap_or_default().to_string_lossy(),
                ));
                if let Err(e) = fs::write(&out_path, &doc.markdown) {
                    eprintln!("write fail {}: {e}", out_path.display());
                    failed += 1;
                    continue;
                }
                println!("✓ {} → {}", path.display(), out_path.display());
                success += 1;
            }
            Err(mdkit::Error::UnsupportedFormat(_)) => {
                // Skip files mdkit doesn't have a backend for.
                // No noisy log — many folders have a mix of
                // extractable and non-extractable files (icons,
                // archives, etc.).
                skipped += 1;
            }
            Err(e) => {
                eprintln!("✗ {}: {e}", path.display());
                failed += 1;
            }
        }
    }

    eprintln!();
    eprintln!("{success} extracted, {skipped} skipped, {failed} failed");
    if failed > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn usage() -> ExitCode {
    eprintln!("usage: batch <input-folder> <output-folder>");
    eprintln!();
    eprintln!("Walks <input-folder> non-recursively, extracts every");
    eprintln!("supported document, writes <name>.<ext>.md per source");
    eprintln!("into <output-folder>. Use scankit for recursive walks.");
    ExitCode::FAILURE
}
