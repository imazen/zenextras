#!/usr/bin/env python3
"""diff_vp8_enc.py — packet-level diff of two vp8-enc-raw-format dumps.

Each dump is a sequence of u32le len | packet bytes | u32le flags.
Reports the first packet that differs and the byte offset within it.
"""
import struct
import sys


def read_pkts(path):
    data = open(path, "rb").read()
    off = 0
    pkts = []
    while off < len(data):
        (n,) = struct.unpack_from("<I", data, off)
        pkt = data[off + 4 : off + 4 + n]
        (flags,) = struct.unpack_from("<I", data, off + 4 + n)
        pkts.append((pkt, flags))
        off += 8 + n
    return pkts


def main(a, b):
    pa, pb = read_pkts(a), read_pkts(b)
    print(f"{a}: {len(pa)} packets; {b}: {len(pb)} packets")
    for i in range(max(len(pa), len(pb))):
        if i >= len(pa) or i >= len(pb):
            print(f"packet {i}: count mismatch ({len(pa)} vs {len(pb)})")
            return 1
        (ba, fa), (bb, fb) = pa[i], pb[i]
        if fa != fb:
            print(f"packet {i}: flag mismatch {fa} vs {fb}")
            return 1
        if ba == bb:
            print(f"packet {i}: identical ({len(ba)} bytes)")
            continue
        # first differing byte
        n = min(len(ba), len(bb))
        d = next((j for j in range(n) if ba[j] != bb[j]), n)
        print(
            f"packet {i}: DIVERGE len {len(ba)} vs {len(bb)}, "
            f"first diff at byte {d} (0x{ba[d:d+1].hex() or 'eof'} vs "
            f"0x{bb[d:d+1].hex() or 'eof'})"
        )
        return 2
    print("ALL IDENTICAL")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1], sys.argv[2]))
