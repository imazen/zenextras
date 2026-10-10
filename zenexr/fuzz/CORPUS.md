# zenexr fuzz corpora

The working corpus is not committed. The repo keeps:

- `fuzz_targets/*.rs`: the harnesses;
- `exr.dict`: libFuzzer dictionary (attribute and type names, magic, version flags);
- `regression/<target>/`: minimized inputs that once failed (none so far).

## Targets

| Target | Description |
|---|---|
| `inventory` | `ExrDecoderConfig::inventory` never panics and always returns a valid inventory |

## Running

Build under the shared build lock, run outside it, niced:

```bash
cd zenexr/fuzz
cargo +nightly fuzz build --target x86_64-unknown-linux-gnu inventory
nice -n 19 ./target/x86_64-unknown-linux-gnu/release/inventory corpus/inventory seeds \
    -max_total_time=600 -dict=exr.dict
```

Seed it with OpenEXR files, for example the OpenEXR sample images
(github.com/AcademySoftwareFoundation/openexr-images), which are test input
only and never committed.
