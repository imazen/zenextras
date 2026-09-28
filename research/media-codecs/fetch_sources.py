"""Retrieve exact upstream source snapshots used by these two probes.

Requires ordinary HTTPS access. Existing modified files are never overwritten.
Snapshots and generated media remain ignored; only manifests belong in Git.
"""
import concurrent.futures
import hashlib
import json
from pathlib import Path
import urllib.request

ROOT = Path(__file__).resolve().parent


def fetch_one(repo, commit, destination, entry):
    relative = Path(entry["path"])
    if relative.is_absolute() or ".." in relative.parts:
        raise ValueError(f"invalid source path: {relative}")
    target = destination / relative
    if target.exists():
        data = target.read_bytes()
    else:
        url = f"https://raw.githubusercontent.com/{repo}/{commit}/{entry['path']}"
        request = urllib.request.Request(url, headers={"User-Agent": "zenextras-codec-qualification"})
        with urllib.request.urlopen(request, timeout=30) as response:
            data = response.read(entry["size"] + 1)
    digest = hashlib.sha1(b"blob " + str(len(data)).encode() + b"\0" + data).hexdigest()
    if digest != entry["git_blob_sha"]:
        raise ValueError(f"source hash mismatch: {target}")
    if not target.exists():
        target.parent.mkdir(parents=True, exist_ok=True)
        # Exclusive creation preserves any concurrent work.
        with target.open("xb") as out:
            out.write(data)


def main():
    for name in ("rust_h264", "ruopus"):
        manifest = json.loads((ROOT / (name + "-source.json")).read_text())
        with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
            futures = [pool.submit(fetch_one, manifest["repository"], manifest["commit"], ROOT / name, entry)
                       for entry in manifest["files"]]
            for future in futures:
                future.result()
        print(f"{name}: verified {len(futures)} source files at {manifest['commit']}")


if __name__ == "__main__":
    main()
