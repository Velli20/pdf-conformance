## Project Overview

pdf-conformance checks [Safe-PDF](https://github.com/Velli20/safe-pdf) against the PDFium and
pdf.js test corpora. PDFium is the reference; pdf.js and WebKit (PDFKit) are shown next to
failing pages for context. Failures are filed as issues in the Safe-PDF repository. See
`README.md`.

- `harness/`: the `pdf-conformance` binary (`cargo conformance …`). It builds Safe-PDF's crates
  by path from `safe-pdf/`, which is gitignored: a symlink to a Safe-PDF checkout, or a clone.
- `pdfkit/render.swift`: the macOS PDFKit renderer for the WebKit context column.
- `.github/workflows/conformance.yml`: triggered by Safe-PDF through `repository_dispatch`.

## Common Commands

```sh
ln -s ~/safe-pdf safe-pdf                   # once: the Safe-PDF checkout to build against
cargo check                                 # Type-check
cargo test                                  # Run tests
cargo clippy --workspace --all-targets      # Lints (CI adds --all-features -- -D warnings)
cargo fmt                                   # Format
cargo conformance fetch --corpus pdfium     # Fetch a corpus at its pinned revision
cargo conformance run --corpus pdfium       # Check a corpus (see README.md for options)
cargo conformance repro <issue key>         # Rerun a Safe-PDF conformance issue's cases
```

## Lint Rules (Critical)

The workspace denies, as Safe-PDF does: `unwrap`/`expect` outside tests, `unsafe_code`,
`indexing_slicing` and `panic`; `as_conversions` and `arithmetic_side_effects` warn.

## Code Style

- Keep Rust modules in flat sibling `.rs` files; no subdirectories beneath `src/`
- Small, composable functions; document public items with `///`
- Unit tests go in `mod tests {}` within the same file
- Issue keys are `SHA-256` of failure signatures. Signatures must not depend on where Safe-PDF
  is checked out, or every open issue in Safe-PDF would be closed and filed again.
