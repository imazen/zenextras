#!/usr/bin/env python3
"""Prepare exported container members against the review API, without sibling patches.

Usage: prepare-container-timeline.py IMMUTABLE_EXPORT NEW_DIRECTORY
This qualifies the two container members, not the other image adapters.
"""
from pathlib import Path
import shutil
import sys
source, destination = (Path(arg).resolve() for arg in sys.argv[1:])
if (source / '.git').exists():
    raise SystemExit('Use an immutable source export')
destination.mkdir(parents=True, exist_ok=False)
for name in ['zenivf', 'zenwebm']:
    shutil.copytree(source / name, destination / name)
shutil.copyfile(source / 'justfile', destination / 'justfile')
manifest = '[workspace]\nmembers = ["zenivf", "zenwebm"]\nresolver = "3"\n\n[patch.crates-io]\n'
for name in ['zencodec', 'zencodec-io', 'zencodec-media', 'zencodec-media-testkit']:
    manifest += f'{name} = {{ git = "https://github.com/imazen/zencodec", rev = "41b1357e9bec50fb1c9508a310ba4b601605207b" }}\n'
manifest += 'zenpixels = { git = "https://github.com/imazen/zenpixels", rev = "f23ac7535be1144a22d0dc6b71f866a08b151f34" }\n'
(destination / 'Cargo.toml').write_text(manifest)
