# Security policy

## Reporting a vulnerability

mdkit parses untrusted input (DOCX, PDF, EPUB, etc.) and dispatches it
to backends including FFI bindings and an external Pandoc binary. If
you find a security issue — memory safety, sandbox escape, denial-of-
service via malformed input, supply-chain concern — please report it
privately first.

**Email:** open a GitHub Security Advisory at
<https://github.com/seryai/mdkit/security/advisories/new>.

We aim to acknowledge reports within **72 hours** and ship a fix or
mitigation within **14 days** for confirmed issues. Reports that meet
the responsible-disclosure criteria below are eligible for credit in
the release notes (and if the project ever has the budget, a bounty).

## Scope

- mdkit's own code (this repository).
- Default-enabled feature flags (the in-process Rust backends).
- Tagged releases on crates.io.

Out of scope:

- Vulnerabilities in third-party backends (Pandoc, Pdfium, ONNX
  Runtime, calamine, etc.) — please report those upstream. We will
  bump our pinned versions promptly when upstream fixes ship.
- Issues that require physical access to the user's machine or
  compromised credentials.

## Responsible-disclosure criteria

- Don't exploit a vulnerability beyond the minimum needed to confirm
  it.
- Don't access, modify, or exfiltrate data that doesn't belong to you.
- Give us reasonable time to ship a fix before public disclosure
  (90 days is the standard window).

## Supported versions

Only the latest published version of mdkit on crates.io receives
security fixes. Once we hit 1.0, we'll commit to supporting the
current minor + the previous one.
