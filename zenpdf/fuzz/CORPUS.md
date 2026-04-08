# zenpdf fuzz corpora

The working corpora for these fuzz targets live in **R2** (Cloudflare),
not in this git repo. See workspace-level docs for the full workflow.

## Targets

| Target | R2 path | Description |
|---|---|---|
| `fuzz_render` | `s3://codec-corpus/fuzz/zenpdf/fuzz_render/` | Full PDF render |
| `fuzz_render_pages` | `s3://codec-corpus/fuzz/zenpdf/fuzz_render_pages/` | Multi-page render |

## Working with the corpus

```bash
# From workspace root
just fuzz-corpus-pull zenpdf
just fuzz zenpdf fuzz_render 300
just fuzz-corpus-cmin zenpdf fuzz_render
```

## Snapshot history

| Date | After cmin | Notes |
|---|---:|---|
| 2026-04-08 | 2,166 / 2,735 | Initial cmin'd snapshot pushed to R2 after merge into zenextras |

## Known crash regression seeds

None yet. 231k iterations clean as of 2026-04-08. See zenpdf CLAUDE.md for
the four known upstream `hayro` issues that should eventually be reproducible
via fuzzing (circular PREV chain, FlateDecode bomb, form XObject recursion,
unbounded image dimensions).
