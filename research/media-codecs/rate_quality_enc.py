#!/usr/bin/env python3
"""rate_quality_enc.py — encode fixtures with zenvp8 and raw libvpx, decode
both, and report stream size + all-plane PSNR vs source.

usage: rate_quality_enc.py <cases.tsv>

Each TSV line: <src.yuv> <w> <h> <qindex> <nframes> <kf_interval> <name>
Requires: zenvp8 enc_raw/dump examples (release, --features encoder),
tools/vp8-enc-raw, tools/vp8-dec-raw.
"""

import math
import struct
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent
OUT = ROOT / "results-vp8-enc" / "rq"
ZENC = ROOT / "zenvp8/target/release/examples/enc_raw"
ZDEC = ROOT / "zenvp8/target/release/examples/dump"
LENC = ROOT / "tools/vp8-enc-raw"
LDEC = ROOT / "tools/vp8-dec-raw"


def psnr_all(src: bytes, dec: bytes, w: int, h: int) -> float:
    """All-plane-average PSNR of two equal-length planar I420 buffers."""
    uw, uh = (w + 1) // 2, (h + 1) // 2
    ysz, csz = w * h, uw * uh
    tot_err = 0
    for off, n in ((0, ysz), (ysz, csz), (ysz + csz, csz)):
        tot_err += sum(
            (a - b) * (a - b) for a, b in zip(src[off : off + n], dec[off : off + n])
        )
    if tot_err == 0:
        return float("inf")
    return 10 * math.log10(255.0 * 255.0 * (ysz + 2 * csz) / tot_err)


def pkts_bytes(path: Path) -> int:
    """Total coded bytes in a u32le len | pkt | u32le flags stream."""
    data = path.read_bytes()
    off = total = 0
    while off < len(data):
        (n,) = struct.unpack_from("<I", data, off)
        total += n
        off += 8 + n
    return total


def run(case: list[str]) -> str:
    src, w, h, q, n, kf, name = case
    w, h, q, n, kf = int(w), int(h), int(q), int(n), int(kf)
    fsz = w * h + 2 * ((w + 1) // 2) * ((h + 1) // 2)
    srcv = Path(src).read_bytes()
    assert len(srcv) == fsz * n, f"{src}: {len(srcv)} != {fsz*n}"

    zp, lp = OUT / f"{name}.z.pkts", OUT / f"{name}.l.pkts"
    zivf, livf = OUT / f"{name}.z.ivf", OUT / f"{name}.l.ivf"
    zdec, ldec = OUT / f"{name}.zdec.yuv", OUT / f"{name}.ldec.yuv"

    with zp.open("wb") as f:
        subprocess.run([ZENC, src, str(w), str(h), str(q), str(n), str(kf), str(zivf)],
                       stdout=f, check=True)
    with lp.open("wb") as f:
        subprocess.run([LENC, src, str(w), str(h), str(q), str(n), str(kf), str(livf)],
                       stdout=f, check=True)
    subprocess.run([ZDEC, str(zivf), str(zdec)], check=True,
                   stdout=subprocess.DEVNULL)
    subprocess.run([LDEC, str(livf), str(ldec)], check=True,
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

    zb, lb = pkts_bytes(zp), pkts_bytes(lp)
    zp_db = psnr_all(srcv, zdec.read_bytes(), w, h)
    lp_db = psnr_all(srcv, ldec.read_bytes(), w, h)
    return (f"{name:<16} q{q:<3} kf{kf:<3} | zenvp8 {zb:>7}B {zp_db:6.2f}dB"
            f" | libvpx {lb:>7}B {lp_db:6.2f}dB | ratio {zb/lb:5.2f}x"
            f" | dPSNR {zp_db-lp_db:+5.2f}")


def main() -> int:
    OUT.mkdir(parents=True, exist_ok=True)
    cases = [
        ln.split()
        for ln in Path(sys.argv[1]).read_text().splitlines()
        if ln.strip() and not ln.startswith("#")
    ]
    for c in cases:
        print(run(c))
    return 0


if __name__ == "__main__":
    sys.exit(main())
