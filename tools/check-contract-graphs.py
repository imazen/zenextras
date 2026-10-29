#!/usr/bin/env python3
"""Check independently resolved root/fuzz graphs for one shared contract family.

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

CORE = "feb09f9fd25e90e49a30d7161db8f3b558dfb2a6"
PIXELS = "283655c5424e0ad7ebc6d2acd94670fac1e29158"
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
        # Explicitly unlock the paired pixels family; Cargo otherwise retains
        # the registry 0.2.16 selection even though the new core needs 0.2.17.
        if lock.exists():
            names = {p["name"] for p in tomllib.loads(lock.read_text())["package"]}
            selected = sorted(names & PIXEL_FAMILY)
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
            if len(entries) != 1:
                raise RuntimeError(f"{directory}: duplicate {crate}: {entries}")
            expected = CORE if crate in FAMILY else PIXELS if crate in PIXEL_FAMILY else None
            if expected and not (entries[0]["source"] or "").endswith("#" + expected):
                raise RuntimeError(f"{directory}: unexpected source for {crate}: {entries}")
        results.append({"graph": directory, "shared": shared})
        (receipts / "graphs.json").write_text(json.dumps(results, indent=2) + "\n")
        print(f"{directory}: one source for each of {len(shared)} present shared crates", flush=True)


if __name__ == "__main__":
    main()
