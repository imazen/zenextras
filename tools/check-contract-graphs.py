#!/usr/bin/env python3
"""Check independently resolved workspace, fuzz and benchmark graphs for the pinned contract families.

Run in an isolated export (outside workspace path overrides) through run-heavy.
Resolution preserves existing lock selections where constraints permit. Keep the
full metadata output and before/after locks in the caller-owned receipt folder.
"""
import argparse
import json
from pathlib import Path
import shutil
import subprocess

CORE = "093211c276fbb1781bec1601d7cb6b0fb92f245a"
PIXELS = "e0dba0052a839ecfe17e4c911a3761e793605adf"
FAMILY = {"zencodec", "zencodec-testkit", "zencodec-io", "zencodec-media",
          "zencodec-media-testkit", "zencodec-coroutine", "zenmeta"}
PIXEL_FAMILY = {"zenpixels", "zenpixels-convert"}
COHORT = {
    "zenjpeg": "0714703dfe94cb20f2bbd84b10a9aab5949782a6",
    "ultrahdr-core": "a9e3eabc0884b03d2d0ef22627b1b41c03b09589",
    "zenpng": "1dd8248fa6683d5c3e22eb946a97b1f93996371a",
    "zenwebp": "3e55db7e5d303fad40ecb14ca396e021397d0230",
}
GRAPHS = [".", "zentiff/fuzz", "zenpdf/fuzz", "zensvg/fuzz", "zenjp2/fuzz", "zenexr/fuzz", "tools/exr-decode-bench"]


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
            if package["name"] in FAMILY | PIXEL_FAMILY | COHORT.keys() | {"how-far"}:
                shared.setdefault(package["name"], []).append({
                    "version": package["version"], "source": package["source"]})
        for crate, entries in shared.items():
            expected = CORE if crate in FAMILY else PIXELS if crate in PIXEL_FAMILY else COHORT.get(crate)
            if len(entries) != 1 or (
                expected and not (entries[0]["source"] or "").endswith("#" + expected)
            ):
                raise RuntimeError(f"{directory}: unexpected source for {crate}: {entries}")
        results.append({"graph": directory, "shared": shared})
        (receipts / "graphs.json").write_text(json.dumps(results, indent=2) + "\n")
        print(f"{directory}: verified sources for {len(shared)} shared crate names", flush=True)


if __name__ == "__main__":
    main()
