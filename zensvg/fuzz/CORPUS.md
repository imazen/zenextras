# zensvg fuzz corpora

The working corpora for these fuzz targets live in **R2** (Cloudflare),
not in this git repo. See workspace-level docs for the full workflow.

## Targets

| Target | R2 path | Description |
|---|---|---|
| `fuzz_parse` | `s3://codec-corpus/fuzz/zensvg/fuzz_parse/` | SVG parsing only (no rendering) |
| `fuzz_render` | `s3://codec-corpus/fuzz/zensvg/fuzz_render/` | Full render pipeline with size limits |

## Working with the corpus

```bash
# From workspace root
just fuzz-corpus-pull zensvg            # download
just fuzz zensvg fuzz_parse 300         # run + auto-sync
just fuzz-corpus-cmin zensvg fuzz_parse # minimize + upload
```

## Snapshot history

| Date | After cmin | Notes |
|---|---:|---|
| 2026-04-08 | 2,928 / 1,885 | Initial cmin'd snapshot pushed to R2 after merge into zenextras |

## Known crash regression seeds

See `regression/fuzz_parse/` and `regression/fuzz_render/`.
Both crashes are giant-percentage attacks (e.g. `100333...333%` in gradient stop offsets).
Likely upstream `usvg`/`resvg` parser DoS — not yet filed.
