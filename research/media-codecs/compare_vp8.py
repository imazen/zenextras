#!/usr/bin/env python3
"""VP8 variant of compare_vp9.py — runs the zenvp8 harness adapter over
results-vp8/*.ivf and compares byte-for-byte against the libvpx reference
(primary oracle). The ffmpeg-native-vp8 reference was already cross-checked
byte-identical at generation time (cases.json "oracles_agree").

Corrupt/derived cases are recorded with whatever status they produce —
"mismatch"/"error" there is evidence, not a regression. Clean encodes must
be "exact".

Usage: compare_vp8.py [--iterations N] [--harness PATH]
"""
import json
import statistics
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent
RESULTS = ROOT / "results-vp8"
HARNESS = ROOT / "harness/target/release/h264-harness"

DECODERS = ["zenvp8"]
# Cases derived to exercise error/robustness paths: divergence from the
# best-effort libvpx reference is expected and recorded, not scored.
DERIVED = {"trunc_tail", "corrupt_mid", "corrupt_kf", "resize_concat"}


def sha256(p: Path) -> str:
    import hashlib
    return hashlib.sha256(p.read_bytes()).hexdigest()


def main() -> int:
    iters = 1
    decoders = DECODERS
    args = sys.argv[1:]
    if "--iterations" in args:
        i = args.index("--iterations")
        iters = int(args[i + 1])
    if "--decoders" in args:
        i = args.index("--decoders")
        decoders = args[i + 1].split(",")
    if "--harness" in args:
        i = args.index("--harness")
        harness = Path(args[i + 1])
    else:
        harness = HARNESS

    cases = [c["name"] for c in
             json.loads((RESULTS / "cases.json").read_text())["cases"]
             if c["status"] == "ok"]
    report = {"harness": str(harness), "cases": []}
    worst = 0
    for dec in decoders:
        for case in cases:
            inp = RESULTS / f"{case}.ivf"
            ref = RESULTS / f"{case}.ref.yuv"
            got = RESULTS / f"{case}.{dec}.got.yuv"
            rec = {"decoder": dec, "case": case,
                   "derived": case in DERIVED}
            if not inp.exists() or not ref.exists():
                rec["status"] = "missing"
                report["cases"].append(rec)
                worst = max(worst, 1)
                continue
            try:
                proc = subprocess.run(
                    [str(harness), dec, str(inp), str(got), str(iters)],
                    capture_output=True, text=True, timeout=300)
            except subprocess.TimeoutExpired:
                rec["status"] = "timeout"
                report["cases"].append(rec)
                worst = max(worst, 1)
                continue
            rec["exit_code"] = proc.returncode
            lines = [l for l in proc.stdout.splitlines() if l.strip().startswith("{")]
            if lines:
                rec["output"] = json.loads(lines[0])
                if len(lines) > 1:
                    rec["measurements"] = [json.loads(l) for l in lines[1:]]
            if proc.returncode != 0:
                err = proc.stderr
                if "panicked at" in err or proc.returncode < 0 or proc.returncode > 128:
                    rec["status"] = "crash"
                    worst = max(worst, 1)
                else:
                    rec["status"] = "error"
                    if case not in DERIVED:
                        worst = max(worst, 1)
                rec["stderr"] = proc.stderr[-2000:]
                report["cases"].append(rec)
                continue
            if not got.exists():
                rec["status"] = "error"
                rec["stderr"] = proc.stderr[-2000:]
                report["cases"].append(rec)
                if case not in DERIVED:
                    worst = max(worst, 1)
                continue
            rb, gb = ref.read_bytes(), got.read_bytes()
            rec["reference_bytes"] = len(rb)
            rec["actual_bytes"] = len(gb)
            rec["reference_sha256"] = sha256(ref)
            rec["got_sha256"] = sha256(got)
            out = rec.get("output") or {}
            if out.get("geometry_changes", 0) > 0:
                rec["status"] = "reinit_ok" if len(gb) > len(rb) else "reinit_wrong"
            elif rb == gb:
                rec["status"] = "exact"
                ms = rec.get("measurements")
                if ms:
                    rec["median_fps"] = statistics.median(
                        m["frames"] * 1e9 / m["ns"] for m in ms)
                    rec["median_first_frame_ms"] = statistics.median(
                        m["first_ns"] / 1e6 for m in ms)
            else:
                n = sum(a != b for a, b in zip(rb, gb)) + abs(len(rb) - len(gb))
                rec["diff_bytes"] = n
                rec["status"] = "mismatch"
                if case not in DERIVED:
                    worst = max(worst, 1)
            report["cases"].append(rec)

    rpath = RESULTS / "compare-vp8.json"
    rpath.write_text(json.dumps(report, indent=1))
    counts = {}
    for c in report["cases"]:
        key = c["decoder"] + ("*" if c.get("derived") else "")
        counts.setdefault(key, {}).setdefault(c["status"], 0)
        counts[key][c["status"]] += 1
    print(json.dumps(counts))
    return worst


if __name__ == "__main__":
    sys.exit(main())
