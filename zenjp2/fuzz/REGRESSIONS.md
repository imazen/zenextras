# Inventory regression provenance

Lossless hexadecimal copies of the existing [PR26 regression files](https://github.com/imazen/zenextras/tree/30c1bc720748839112f9426dadf00cf45f6fa31c/zenjp2/fuzz/regression/inventory). Stable tests decode the text before running the original assertions and decoder checks.

| File | Decoded bytes | SHA-256 |
| --- | --- | --- |
| `emptypart-invalid-siz.hex` | 103 | `e593d77fcb9b7903c58eafeaa498bfba99bc9272602684adf931e17d7fcafe18` |
| `emptypart-psot0-eoc-ppm.hex` | 141 | `a4ba9ea980bccef3ba72b8f6cfda534ac114c0d2b8a4915d8477d701026bbe93` |
| `timeout-skinny-tag-trees.hex` | 206 | `15b13998133b5a06e43cdacb03e93b79b9cc85aa19d4f6cb4c91352306fa5eb0` |
