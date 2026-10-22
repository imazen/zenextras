#!/usr/bin/env python3
"""Summarize exr-decode-bench `run` CSVs (standard library only).

usage: summarize.py results.csv[,more.csv] [contention.log[,more.log] [min-cpu-percent [rerun-list]]] > summary.md

Per file and arm, the value is the lower of the per-round medians (the round
least disturbed by other load). A round's rows are discarded when its process
window overlaps a contention-log sample of another process at or above
min-cpu-percent (default 50, i.e. half a core of real work; the watcher logs
from 15%). A file is dropped, and listed, unless every arm kept a clean round;
re-run those files and pass both CSVs.
"""

import bisect
import csv
import math
import sys
from collections import defaultdict


def size_class(w, h):
    px = w * h
    if px <= 64 * 64:
        return "tiny 64x64"
    if px <= 256 * 256:
        return "small 256x256"
    if px <= 1100 * 1100:
        return "medium 0.24-1.1 MP"
    return "large 4096x2048"


SIZE_ORDER = ["tiny 64x64", "small 256x256", "medium 0.24-1.1 MP", "large 4096x2048"]
COMP_ORDER = [
    "none",
    "zip",
    "piz",
    "b44",
    "b44a",
    "dwaa_15",
    "dwaa_45",
    "dwaa_150",
    "dwab_15",
    "dwab_45",
    "dwab_150",
]


def geomean(values):
    return math.exp(sum(math.log(v) for v in values) / len(values))


def load(paths, contention_paths, min_cpu):
    rows = [r for path in paths.split(",") for r in csv.DictReader(open(path))]
    contention = []
    for contention_path in filter(None, (contention_paths or "").split(",")):
        for line in open(contention_path):
            parts = line.split()
            if parts and float(parts[2]) >= min_cpu:
                contention.append(int(parts[0]))
    contention.sort()
    starts = sorted({int(r["unix_ms"]) for r in rows})
    next_start = {s: e for s, e in zip(starts, starts[1:] + [starts[-1] + 60_000])}

    def contended(s, e):
        # A contention sample at t covers roughly [t, t + 2000).
        i = bisect.bisect_right(contention, s - 2000)
        return i < len(contention) and contention[i] < e

    best = {}
    arms_seen = set()
    for r in rows:
        key = (r["image"], r["width"], r["height"], r["encoder"], r["compression"], r["level"])
        arms_seen.add(r["arm"])
        s = int(r["unix_ms"])
        if contended(s, next_start[s]):
            continue
        k = key + (r["threads"], r["arm"])
        ns = int(r["median_ns"])
        if k not in best or ns < best[k]["ns"]:
            w, h = int(r["width"]), int(r["height"])
            best[k] = {
                "key": key,
                "image": r["image"],
                "w": w,
                "h": h,
                "px": w * h,
                "encoder": r["encoder"],
                "comp": r["compression"] + (f"_{r['level']}" if r["level"] else ""),
                "threads": int(r["threads"]),
                "arm": r["arm"],
                "ns": ns,
                "bpp": float(r["bits_per_pixel"]),
            }
    # A file counts only when every arm kept at least one clean round.
    arms_by_key = defaultdict(set)
    for v in best.values():
        arms_by_key[v["key"]].add(v["arm"])
    all_keys = {(r["image"], r["width"], r["height"], r["encoder"], r["compression"], r["level"]) for r in rows}
    dropped = {k for k in all_keys if arms_by_key.get(k, set()) != arms_seen}
    return [v for v in best.values() if v["key"] not in dropped], dropped


def table(header, rows):
    out = ["| " + " | ".join(header) + " |", "|" + "|".join("---" for _ in header) + "|"]
    out += ["| " + " | ".join(str(c) for c in row) + " |" for row in rows]
    return "\n".join(out)


def main():
    data, dropped = load(
        sys.argv[1],
        sys.argv[2] if len(sys.argv) > 2 else "",
        float(sys.argv[3]) if len(sys.argv) > 3 else 50.0,
    )
    threads = sorted({d["threads"] for d in data})
    arms = sorted({d["arm"] for d in data}, key=lambda a: (not a.startswith("openexr"), a))
    print(f"# exr decode benchmark summary ({sys.argv[1]}, threads {threads})\n")
    print(f"{len(data)} file x arm values; {len(dropped)} files dropped for contention.\n")
    if dropped:
        print("Dropped: " + ", ".join(sorted("/".join(k) for k in dropped)) + "\n")
        if len(sys.argv) > 4:
            # Re-run list for `exr-decode-bench run ... @list`.
            with open(sys.argv[4], "w") as f:
                for image, w, h, enc, comp, level in sorted(dropped):
                    name = f"{enc}_{comp}" + (f"_{level}" if level else "")
                    f.write(f"{image}/{w}x{h}/{name}.exr\n")

    # 1. Geomean MP/s per size class, compression and arm (OpenEXR-encoded inputs).
    print("## Geomean MP/s, OpenEXR-encoded inputs\n")
    print("Each cell is the geometric mean over images of pixels / median decode time.\n")
    groups = defaultdict(list)
    for d in data:
        if d["encoder"] == "openexr":
            groups[(size_class(d["w"], d["h"]), d["comp"], d["arm"])].append(d["px"] / d["ns"] * 1e3)
    for size in SIZE_ORDER:
        rows = []
        for comp in COMP_ORDER:
            cells = []
            n = 0
            for arm in arms:
                v = groups.get((size, comp, arm))
                if v:
                    n = len(v)
                cells.append(f"{geomean(v):.0f}" if v else "")
            if any(cells):
                rows.append([comp, n] + cells)
        if rows:
            print(f"### {size}\n")
            print(table(["compression", "images"] + arms, rows) + "\n")

    # 2. Ratio OpenEXR / Rust per compression (all sizes, OpenEXR-encoded).
    pairs = [
        ("openexr-f16-planar", "exr-blocks-f16-planar"),
        ("openexr-f16-planar", "exr-blocks-f16-planar-pool"),
        ("openexr-f16-planar", "exr-f16-planar"),
        ("openexr-f32-rgb", "exr-f32-rgb"),
        ("openexr-f32-rgb", "zenexr-f32-rgb"),
    ]
    pairs = [p for p in pairs if p[0] in arms and p[1] in arms]
    print("## Speed ratio: Rust time / OpenEXR time (>1 means OpenEXR is faster)\n")
    print("Geomean over every OpenEXR-encoded file at that size of the per-file time ratio.\n")
    by_file = defaultdict(dict)
    for d in data:
        by_file[(d["image"], d["w"], d["h"], d["encoder"], d["comp"])][d["arm"]] = d["ns"]
    for size in SIZE_ORDER:
        rows = []
        for comp in COMP_ORDER:
            cells = []
            for ref, cand in pairs:
                ratios = [
                    arms_[cand] / arms_[ref]
                    for (img, w, h, enc, c), arms_ in by_file.items()
                    if enc == "openexr" and c == comp and size_class(w, h) == size
                    and ref in arms_ and cand in arms_
                ]
                cells.append(f"{geomean(ratios):.2f}x" if ratios else "")
            if any(cells):
                rows.append([comp] + cells)
        if rows:
            print(f"### {size}\n")
            print(table(["compression"] + [f"{c} / {r}" for r, c in pairs], rows) + "\n")

    # 3. Per-call time at 64x64. A linear alpha + beta * pixels fit does not
    # hold here: output beyond L3 (the 4096x2048 frames) drops throughput.
    print("## Per-call time at 64x64 (geomean us, OpenEXR-encoded): the fixed cost\n")
    rows = []
    for comp in COMP_ORDER:
        cells = []
        for arm in arms:
            v = [d["ns"] / 1e3 for d in data if d["encoder"] == "openexr" and d["comp"] == comp
                 and d["arm"] == arm and d["px"] == 64 * 64]
            cells.append(f"{geomean(v):.0f}" if v else "")
        if any(cells):
            rows.append([comp] + cells)
    print(table(["compression"] + arms, rows) + "\n")

    # 4. Encoder effect: same pixels, exr-encoded vs OpenEXR-encoded bitstreams.
    print("## Encoder effect: time(exr-encoded file) / time(OpenEXR-encoded file), same decoder\n")
    rows = []
    for comp in ["b44", "b44a", "dwaa_45", "dwab_45"]:
        cells = []
        for arm in arms:
            ratios = []
            for (img, w, h, enc, c), arms_ in by_file.items():
                if enc != "exr" or c != comp or arm not in arms_:
                    continue
                other = by_file.get((img, w, h, "openexr", c), {})
                if arm in other:
                    ratios.append(arms_[arm] / other[arm])
            cells.append(f"{geomean(ratios):.2f}x (n={len(ratios)})" if ratios else "")
        rows.append([comp] + cells)
    print(table(["compression"] + arms, rows) + "\n")

    # 5. Compressed size, for context.
    print("## Compressed size, OpenEXR encoder (geomean bits per pixel, RGB half = 48 bpp)\n")
    rows = []
    for comp in COMP_ORDER:
        v = {(d["image"], d["w"], d["h"]): d["bpp"] for d in data if d["encoder"] == "openexr" and d["comp"] == comp}
        if v:
            rows.append([comp, len(v), f"{geomean(list(v.values())):.2f}"])
    print(table(["compression", "files", "bpp"], rows))


if __name__ == "__main__":
    main()
