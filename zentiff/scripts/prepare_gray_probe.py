#!/usr/bin/env python3
"""Build a no-JPEG validation manifest while the full cohort cannot resolve.

The source/test directories remain symlinks to the real crate. This validates the
selected TIFF path, not the root workspace or its optional JPEG dependency.
"""
import argparse, re
from pathlib import Path
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("output", type=Path)
args = parser.parse_args()
crate = Path(__file__).resolve().parents[1]
root = crate.parent
out = args.output.resolve()
out.mkdir(parents=True, exist_ok=False)
manifest = (crate / "Cargo.toml").read_text()
manifest = re.sub(r'^zenjpeg = \{[^\n]*\}\n', '', manifest, flags=re.M)
manifest = re.sub(r'^zenjpeg = \[[^\n]*\]', 'zenjpeg = []', manifest, flags=re.M)
patches = (root / "Cargo.toml").read_text().split('[patch.crates-io]', 1)[1]
patches = '\n'.join(line for line in patches.splitlines() if not line.startswith(
    ('zenjpeg =', 'zenresize =', 'zenpng =', 'ultrahdr-core =', 'zenflate =', 'hayro-syntax =')))
(out / "Cargo.toml").write_text(manifest + '\n[workspace]\n\n[patch.crates-io]\n' + patches + '\n')
for item in ['src', 'tests', 'benches', 'examples']:
    (out / item).symlink_to(crate / item, target_is_directory=True)
print(out / "Cargo.toml")
