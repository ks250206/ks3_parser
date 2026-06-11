# AGENTS.md

## Project Rules

- This repository is `ks3_parser`, a Rust parser/converter for Kyowa Standard Format 3 `.KS3` analog data.
- Preserve the `ks2_parser`-derived CLI/TUI experience, but keep KS3 conversion metadata-driven.
- Do not reintroduce KS2 manual conversion settings such as `ADConverterScale`, `ADRangeCoefficient`, `ADCoefficient`, or `coefficient.CH1`-`CH4` unless the task explicitly asks for an override feature.
- KS3 coefficients are parsed from the input file:
  - `0x0020/0x0019`: range conversion coefficient
  - `0x0020/0x001A`: engineering conversion coefficient
  - `0x0020/0x0022`: cable coefficient
  - `0x0020/0x0023`: arbitrary correction coefficient
  - `0x0020/0x0020`, `0x0021`: offset and offset-zero value
- Unknown KS3 major/minor items must be ignored unless they are required for the requested feature.
- Output CSV must remain Shift-JIS, CRLF, and compatible with the sample CSV format unless the task explicitly changes the output contract.

## Files and Data

- `samples/` is local verification data only and must not be committed.
- `ks3_format.md` and `ks3_format.pdf` are local references only and must not be committed.
- Keep `config.toml` minimal: input path, output directory, and output file name.
- Keep release optimization settings in `Cargo.toml` unless there is a measured reason to change them.

## Verification

Run these before finishing parser or output changes:

```bash
cargo fmt
cargo test --locked
cargo build --release --locked
cargo llvm-cov --workspace --all-features --locked --fail-under-lines 80
```

When local samples are available, also run:

```bash
KS3_SAMPLE_DIR=samples cargo test local_samples_match_reference_csv -- --ignored
```

## Release

- Release tags use `v*` and trigger `.github/workflows/release.yml`.
- The release workflow builds Apple Silicon macOS, Intel macOS, and Windows x64 zip assets.
- Before tagging, verify the working tree is clean except for intentionally ignored local files.
