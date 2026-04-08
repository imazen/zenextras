# zentiff fuzz corpora

The working corpora for these fuzz targets live in **R2** (Cloudflare),
not in this git repo. The repo only commits:

- `fuzz_targets/*.rs` — the harnesses
- `tiff.dict` — libFuzzer dictionary
- `regression/<target>/` — crashes that ever fired, kept forever

## Targets

| Target | R2 path | Description |
|---|---|---|
| `fuzz_decode` | `s3://codec-corpus/fuzz/zentiff/fuzz_decode/` | Full TIFF decode pipeline |
| `fuzz_decode_limits` | `s3://codec-corpus/fuzz/zentiff/fuzz_decode_limits/` | Decode with strict resource limits |
| `fuzz_probe` | `s3://codec-corpus/fuzz/zentiff/fuzz_probe/` | Header-only metadata probe |

## Working with the corpus

From the workspace root:

```bash
# Pull the latest corpus before fuzzing
just fuzz-corpus-pull zentiff

# Run a fuzz target (auto-pulls, runs, auto-pushes new finds)
just fuzz zentiff fuzz_decode 300

# Minimize and re-upload
just fuzz-corpus-cmin zentiff fuzz_decode

# Show what differs between local and R2
just fuzz-corpus-diff zentiff
```

Required environment variables (already set in lilith's shell):

- `R2_ACCOUNT_ID`
- `R2_BUCKET=codec-corpus`
- `R2_ACCESS_KEY_ID`
- `R2_SECRET_ACCESS_KEY`

## Snapshot history

| Date | Files | After cmin | Notes |
|---|---:|---:|---|
| 2026-04-08 | — | 1,556 / 1,364 / 718 | Initial cmin'd snapshot pushed to R2 after merge into zenextras |

## Known crash regression seeds

See `regression/fuzz_decode/` and `regression/fuzz_decode_limits/`.
The most interesting one is `oom-ec38252b...` (BigTIFF + GDAL_STRUCTURAL_METADATA, 20.5 KB)
which is a real-world geospatial TIFF pattern that triggered OOM.
