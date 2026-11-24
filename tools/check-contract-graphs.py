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
import tomllib

CORE = "f02a17ec6001a9efcad1267f92a3ea928ed06e57"
PIXELS = "283655c5424e0ad7ebc6d2acd94670fac1e29158"
FAMILY = {"zencodec", "zencodec-testkit", "zencodec-io", "zencodec-media",
          "zencodec-media-testkit", "zencodec-coroutine", "zenmeta"}
NATIVE_PIXELS = "42a0c5c83c5c418950eb528d92d57788f6700bcf"
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
        # Explicitly unlock the paired pixels family; Cargo otherwise retains
        # the registry 0.2.16 selection even though the new core needs 0.2.17.
        if lock.exists():
            packages = tomllib.loads(lock.read_text())["package"]
            selected = sorted(
                f"{p['name']}@{p['version']}" for p in packages if p["name"] in PIXEL_FAMILY
            )
            if selected:
                update = ["cargo", "update", "--manifest-path", str(root / directory / "Cargo.toml")]
                for crate in selected:
                    update += ["-p", crate]
                with (out / "paired-update.log").open("w") as log:
                    subprocess.run(update, cwd=root / directory, stdout=log, stderr=log,
                                   check=True, timeout=600)
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
            expected = [CORE] if crate in FAMILY else [PIXELS] if crate in PIXEL_FAMILY else [None]
            if crate == "zenpixels" and directory != "zensvg/fuzz":
                expected.append(NATIVE_PIXELS)
            sources = [(entry["source"] or "").split("#")[-1] for entry in entries]
            if len(sources) != len(expected) or any(
                rev is not None and rev not in sources for rev in expected
            ):
                raise RuntimeError(f"{directory}: unexpected source for {crate}: {entries}")
        results.append({"graph": directory, "shared": shared})
        (receipts / "graphs.json").write_text(json.dumps(results, indent=2) + "\n")
        print(f"{directory}: verified sources for {len(shared)} shared crate names", flush=True)


if __name__ == "__main__":
    main()
