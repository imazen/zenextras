#!/usr/bin/env python3
"""Check independently resolved root/fuzz graphs for the pinned contract families.

Run in an isolated export (outside workspace path overrides) through run-heavy.
Resolution preserves existing lock selections where constraints permit. Keep the
full metadata output and before/after locks in the caller-owned receipt folder.
"""
import argparse
import json
from pathlib import Path
import shutil
import subprocess

CORE = "037ef59b57ae003f853a5b6624b173edfee22d6a"
PIXELS = "42a0c5c83c5c418950eb528d92d57788f6700bcf"
FAMILY = {"zencodec", "zencodec-testkit", "zencodec-io", "zencodec-media",
          "zencodec-media-testkit", "zencodec-coroutine", "zenmeta"}
PIXEL_FAMILY = {"zenpixels", "zenpixels-convert"}
GRAPHS = [".", "zentiff/fuzz", "zenpdf/fuzz", "zensvg/fuzz", "zenjp2/fuzz", "zenexr/fuzz"]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("receipts", type=Path)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    receipts = args.receipts.resolve()
    receipts.mkdir(parents=True, exist_ok=False)
    results = []
    for directory in GRAPHS:
        name = "workspace" if directory == "." else directory.replace("/", "-")
        out = receipts / name
        out.mkdir()
        lock = root / directory / "Cargo.lock"
        if lock.exists():
            shutil.copyfile(lock, out / "before.lock")
        command = ["cargo", "metadata", "--all-features", "--format-version", "1",
                   "--manifest-path", str(root / directory / "Cargo.toml")]
        with (out / "metadata.json").open("w") as stdout, (out / "resolution.log").open("w") as stderr:
            subprocess.run(command, cwd=root / directory, stdout=stdout, stderr=stderr,
                           check=True, timeout=600)
        shutil.copyfile(lock, out / "after.lock")
        packages = json.loads((out / "metadata.json").read_text())["packages"]
        shared = {}
        for package in packages:
            if package["name"] in FAMILY | PIXEL_FAMILY | {"how-far"}:
                shared.setdefault(package["name"], []).append({
                    "version": package["version"], "source": package["source"]})
        for crate, entries in shared.items():
            expected = CORE if crate in FAMILY else PIXELS if crate in PIXEL_FAMILY else None
            if len(entries) != 1 or (
                expected and not (entries[0]["source"] or "").endswith("#" + expected)
            ):
                raise RuntimeError(f"{directory}: unexpected source for {crate}: {entries}")
        results.append({"graph": directory, "shared": shared})
        (receipts / "graphs.json").write_text(json.dumps(results, indent=2) + "\n")
        print(f"{directory}: verified sources for {len(shared)} shared crate names", flush=True)


if __name__ == "__main__":
    main()
