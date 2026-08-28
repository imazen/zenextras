# Independent fixture

`rgb-half-piz.exr`: 31×33 RGB half samples, PIZ compression, origin (0,0).
Generated with OpenEXR 3.1.11 by the prior zenbitmaps port-validation generator,
September 8, 2026. `rgb-half-piz.f32le` contains the generator's original input
samples, widened to f32, tightly packed RGB, 372 bytes per row, little-endian.
The expected pixels were not produced by the `exr` decoder being tested.

SHA-256:

- EXR: `5b3b86c3b24e98dfff3e184a635ba164bad5028127305b85e817fc06a4a41e23`
- f32le: `862ebeee902ce3113818752108c1a6f2320c1f25f83eebe91587aa3b24957951`

Generation source is preserved in checkpoint
`f6140cb736e395cd70b8cf381caf50a3d6352cdf`, `tests/reference/exr_fixture.cpp`,
inside `/mnt/v/output/zensim/native-exr-port-2026-09-08/source-code.tar.gz`.
Original names: `31x33-m0-c4-p0-o0.{exr,rgbf32}`. The larger 98-file corpus is
external; only this small independent regression fixture is committed.
