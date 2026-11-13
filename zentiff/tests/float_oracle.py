"""Read authored float TIFF witnesses independently with tifffile/imagecodecs."""
import pathlib
import sys

import numpy as np
import tifffile

root = pathlib.Path(sys.argv[1])
files = sorted(root.glob("*.tif"))
if len(files) != 64:
    raise SystemExit(f"expected 64 TIFF witnesses, found {len(files)}")
for path in files:
    expected = np.frombuffer(path.with_suffix(".raw").read_bytes(), dtype=np.uint32)
    actual = tifffile.imread(path)
    if actual.dtype.kind != "f" or actual.dtype.itemsize != 4:
        raise AssertionError((path, actual.dtype))
    actual_bits = actual.astype(np.float32, copy=False).view(np.uint32).ravel()
    if not np.array_equal(actual_bits, expected):
        raise AssertionError(f"float sample bits differ: {path}")
print(f"{len(files)} TIFFs decode with exact float bits through tifffile/imagecodecs")
