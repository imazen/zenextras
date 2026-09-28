#!/usr/bin/env python3
"""Audio candidate qualification matrix.

Drives audio-harness subcommands (one child process per cell, crash-isolated)
and classifies results:

  exact        — decoded PCM bit-identical / count+content verified (FLAC)
  count_exact  — decoded samples-per-ch == padded samples-per-ch (Opus packet API)
  snr_db       — SNR vs padded input at best ±960-sample alignment
  ref_snr_db   — SNR of candidate decode vs libopus decode of same packets
  rejected     — clean error return (unsupported/malformed)
  crash        — nonzero exit / signal
  error        — other failure

AAC note: ADTS has no priming/end-trim fields; counts are reported raw and
aligned-SNR is the correctness metric (vs ffmpeg's own decode).
"""
import hashlib
import json
import math
import struct
import subprocess
import sys
import wave
from pathlib import Path

ROOT = Path(__file__).resolve().parent
BIN = ROOT / "audio-harness" / "target" / "release" / "audio-harness"
OUT = ROOT / "audio-results" / "matrix"
OUT.mkdir(parents=True, exist_ok=True)

OPUS_DECS = ["ruopus", "opus-rs", "rusopus", "libopus"]
OPUS_ENCS = ["ruopus", "opus-rs"]


def run(args, timeout=120):
    p = subprocess.run(list(map(str, args)), stdout=subprocess.PIPE,
                       stderr=subprocess.PIPE, timeout=timeout)
    try:
        j = json.loads(p.stdout.decode().strip().splitlines()[-1])
    except Exception:
        j = {"status": "crash" if p.returncode else "error",
             "exit": p.returncode,
             "stdout": p.stdout.decode(errors="replace")[-300:],
             "stderr": p.stderr.decode(errors="replace")[-500:]}
    if p.returncode not in (0, 2):
        j["status"] = "crash"
        j["exit"] = p.returncode
        j["stderr"] = p.stderr.decode(errors="replace")[-500:]
    j["_exit"] = p.returncode
    return j


def read_f32(p):
    b = Path(p).read_bytes()
    return struct.unpack(f"<{len(b)//4}f", b)


def snr_at_best_lag(ref, got, max_lag=960):
    """SNR(dB) of `got` vs `ref` at the best integer lag in ±max_lag."""
    n = min(len(ref), len(got))
    if n < 128:
        return None, 0
    best = (-1e30, 0)
    # coarse scan then refine
    for lag in range(-max_lag, max_lag + 1, 8):
        if lag >= 0:
            a, b = ref[lag:lag + n], got[:n - lag] if n - lag > 0 else []
        else:
            a, b = ref[:n + lag], got[-lag:-lag + n + lag]
        m = min(len(a), len(b))
        if m < 128:
            continue
        num = sum(x * y for x, y in zip(a[:m], b[:m]))
        den = sum(y * y for y in b[:m]) + 1e-12
        c = num / den
        if c > best[0]:
            best = (c, lag)
    lag = best[1]
    for l2 in range(lag - 8, lag + 9):
        if abs(l2) > max_lag:
            continue
        if l2 >= 0:
            a, b = ref[l2:l2 + n], got[:n - l2]
        else:
            a, b = ref[:n + l2], got[-l2:-l2 + n + l2]
        m = min(len(a), len(b))
        if m < 128:
            continue
        num = sum(x * y for x, y in zip(a[:m], b[:m]))
        den = sum(y * y for y in b[:m]) + 1e-12
        if num / den > best[0]:
            best = (num / den, l2)
    lag = best[1]
    if lag >= 0:
        a, b = ref[lag:], got[:len(ref) - lag]
    else:
        a, b = ref[:len(got) + lag], got[-lag:]
    m = min(len(a), len(b))
    a, b = a[:m], b[:m]
    sig = sum(x * x for x in a)
    err = sum((x - y) ** 2 for x, y in zip(a, b))
    if err == 0:
        return 300.0, lag
    if sig == 0:
        return None, lag
    return 10 * math.log10(sig / err), lag


def gen_pcm(samples, ch):
    """Tone + end impulse, interleaved f32."""
    out = []
    for i in range(samples):
        for c in range(ch):
            v = 0.2 * math.sin(i * (440 + 170 * c) * 2 * math.pi / 48000)
            out.append(0.8 if i == samples - 1 and c == 0 else v)
    return out


def write_f32(path, pcm):
    Path(path).write_bytes(b"".join(struct.pack("<f", s) for s in pcm))


def write_s16(path, pcm16):
    Path(path).write_bytes(b"".join(struct.pack("<h", s) for s in pcm16))


results = {"cases": [], "opus": {}, "aac": {}, "flac": {}, "malformed": {}}

# ── Opus round-trip grid ─────────────────────────────────────────────────
mono_sizes = [1, 119, 120, 121, 480, 481, 959, 960, 961, 1080, 1920, 1921, 4800, 48000]
stereo_sizes = [120, 960, 961, 4800, 48000]
opus_rows = []
for ch, sizes in ((1, mono_sizes), (2, stereo_sizes)):
    for n in sizes:
        pcm = gen_pcm(n, ch)
        inp = OUT / f"in-n{n}-ch{ch}.f32le"
        write_f32(inp, pcm)
        for enc in OPUS_ENCS:
            pktdir = OUT / f"pkts-{enc}-n{n}-ch{ch}"
            e = run([BIN, "opus-enc", enc, inp, ch, 64000, pktdir])
            if e["status"] != "ok":
                opus_rows.append({"case": f"n{n}-ch{ch}", "enc": enc, "status": e["status"],
                                  "error": e.get("error")})
                continue
            pkts = sorted(pktdir.glob("pkt_*.bin"))
            padded = e["padded_per_ch"]
            for dec in OPUS_DECS:
                outp = OUT / f"dec-{dec}-{enc}-n{n}-ch{ch}.f32le"
                d = run([BIN, "opus-dec", dec, ch, outp, *pkts])
                row = {"case": f"n{n}-ch{ch}", "enc": enc, "dec": dec,
                       "input_per_ch": n, "padded_per_ch": padded}
                if d["status"] != "ok":
                    row["status"] = d["status"]
                    row["error"] = d.get("error") or d.get("stderr")
                else:
                    got = read_f32(outp)
                    # per-channel view of channel 0 for SNR
                    got0 = got[0::ch]
                    ref0 = pcm[0::ch] + [0.0] * (padded - n)
                    row["decoded_per_ch"] = d["decoded_per_ch"]
                    row["count_exact"] = d["decoded_per_ch"] == padded
                    if n >= 480 and len(got0) >= n:
                        s, lag = snr_at_best_lag(ref0, got0)
                        row["snr_db"] = None if s is None else round(s, 1)
                        row["lag"] = lag
                    row["status"] = "ok"
                opus_rows.append(row)
                print(json.dumps({k: row[k] for k in ("case", "enc", "dec", "status") if k in row}
                                 | {"cnt": row.get("decoded_per_ch"), "exp": padded,
                                    "snr": row.get("snr_db")}), flush=True)

# Cross-decode check: every impl's decode of identical packets vs libopus.
for row in opus_rows:
    if row.get("status") != "ok" or row["dec"] == "libopus" or row["input_per_ch"] < 480:
        continue
    ref_f = OUT / f"dec-libopus-{row['enc']}-n{row['input_per_ch']}-ch{row['case'][-4:]}.f32le"
    # find matching libopus row
    lib = [r for r in opus_rows if r["case"] == row["case"] and r["enc"] == row["enc"]
           and r["dec"] == "libopus" and r.get("status") == "ok"]
    if not lib:
        continue
    ch = 2 if row["case"].endswith("ch2") else 1
    a = read_f32(OUT / f"dec-{row['dec']}-{row['enc']}-n{row['input_per_ch']}-ch{ch}.f32le")
    b = read_f32(OUT / f"dec-libopus-{row['enc']}-n{row['input_per_ch']}-ch{ch}.f32le")
    if len(a) == len(b) and len(a) > 0:
        err = sum((x - y) ** 2 for x, y in zip(a, b))
        sig = sum(x * x for x in b)
        row["ref_snr_db"] = 300.0 if err == 0 else round(10 * math.log10((sig + 1e-12) / err), 1)

results["opus"] = opus_rows

# ── ruopus Ogg helper layer (distinct container defect tracking) ──────────
ogg_rows = []
for ch in (1, 2):
    for br in (24000, 64000):
        for n in (1, 120, 959, 960, 961, 1920, 1921):
            pcm = gen_pcm(n, ch)
            inp = OUT / f"ogg-n{n}-ch{ch}.f32le"
            write_f32(inp, pcm)
            r = run([BIN, "ogg-rt", inp, ch, br])
            r.update({"case": f"n{n}-ch{ch}-br{br}"})
            ogg_rows.append(r)
            print(json.dumps({"ogg": r["case"], "in": r.get("input_per_ch"),
                              "out": r.get("decoded_per_ch"), "st": r["status"]}), flush=True)
results["ogg_helpers"] = ogg_rows

# ── AAC: ffmpeg-encoded ADTS → oxideav decode ────────────────────────────
aac_rows = []
for ch, rate, n, name in ((2, 44100, 44100, "stereo44"), (1, 48000, 48000, "mono48"),
                          (2, 48000, 96000, "stereo48-2s")):
    wav = OUT / f"aac-src-{name}.wav"
    adts = OUT / f"aac-{name}.adts"
    refpcm = OUT / f"aac-ref-{name}.f32le"
    subprocess.run(["ffmpeg", "-v", "error", "-y", "-f", "lavfi",
                    "-i", f"sine=frequency=440:sample_rate={rate}:duration={n/rate}",
                    "-ac", str(ch), str(wav)], check=True)
    subprocess.run(["ffmpeg", "-v", "error", "-y", "-i", str(wav),
                    "-c:a", "aac", "-b:a", "96000", "-f", "adts", str(adts)], check=True)
    subprocess.run(["ffmpeg", "-v", "error", "-y", "-i", str(adts),
                    "-f", "f32le", "-acodec", "pcm_f32le", str(refpcm)], check=True)
    outp = OUT / f"aac-dec-{name}.f32le"
    d = run([BIN, "aac-dec", adts, outp])
    row = {"case": name, "rate": rate, "ch": ch, "status": d["status"]}
    if d["status"] == "ok":
        row["decoded_per_ch"] = d["decoded_per_ch"]
        got = read_f32(outp)
        ref = read_f32(refpcm)
        got0, ref0 = got[0::ch], ref[0::ch]
        s, lag = snr_at_best_lag(ref0, got0, max_lag=4096)
        row["snr_db_vs_ffmpeg"] = None if s is None else round(s, 1)
        row["lag"] = lag
        row["ffmpeg_per_ch"] = len(ref0)
    else:
        row["error"] = d.get("error") or d.get("stderr")
    aac_rows.append(row)
    print(json.dumps(row), flush=True)

# AAC encode direction: s16 → oxideav aac-enc → ffmpeg decode → SNR.
for ch, rate, n, name in ((2, 48000, 48000, "enc-stereo48"),):
    wav = OUT / f"aac-enc-src-{name}.wav"
    subprocess.run(["ffmpeg", "-v", "error", "-y", "-f", "lavfi",
                    "-i", f"sine=frequency=440:sample_rate={rate}:duration={n/rate}",
                    "-ac", str(ch), str(wav)], check=True)
    w = wave.open(str(wav))
    pcm16 = w.readframes(w.getnframes())
    s16 = OUT / f"aac-enc-{name}.s16le"
    s16.write_bytes(pcm16)
    adts = OUT / f"aac-enc-{name}.adts"
    e = run([BIN, "aac-enc", s16, ch, rate, 96000, adts])
    row = {"case": name, "status": e["status"], "adts_bytes": e.get("adts_bytes")}
    if e["status"] == "ok":
        dec = OUT / f"aac-enc-{name}.dec.f32le"
        subprocess.run(["ffmpeg", "-v", "error", "-y", "-i", str(adts),
                        "-f", "f32le", "-acodec", "pcm_f32le", str(dec)])
        got = read_f32(dec) if dec.exists() else []
        orig = struct.unpack(f"<{len(pcm16)//2}h", pcm16)
        orig = [s / 32768.0 for s in orig]
        got0, ref0 = got[0::ch], orig[0::ch]
        s, lag = snr_at_best_lag(ref0, got0, max_lag=8192)
        row["decoded_per_ch_by_ffmpeg"] = len(got0)
        row["snr_db_via_ffmpeg"] = None if s is None else round(s, 1)
        row["lag"] = lag
    else:
        row["error"] = e.get("error") or e.get("stderr")
    aac_rows.append(row)
    print(json.dumps(row), flush=True)
results["aac"] = aac_rows

# ── FLAC: bit-exact round trip via ffmpeg decode ──────────────────────────
flac_rows = []
for ch, rate, n, bits in ((1, 48000, 48000, 16), (2, 44100, 44100, 16),
                          (1, 48000, 999, 16), (2, 48000, 4097, 16)):
    pcm = gen_pcm(n, ch)
    s16 = [max(-32768, min(32767, int(round(v * 32767)))) for v in pcm]
    inp = OUT / f"flac-n{n}-ch{ch}.s16le"
    write_s16(inp, s16)
    outp = OUT / f"flac-n{n}-ch{ch}.flac"
    e = run([BIN, "flac-enc", inp, ch, rate, bits, outp])
    row = {"case": f"n{n}-ch{ch}-{bits}b", "status": e["status"]}
    if e["status"] == "ok":
        row["flac_bytes"] = e["flac_bytes"]
        dec = OUT / f"flac-n{n}-ch{ch}.dec.s16le"
        p = subprocess.run(["ffmpeg", "-v", "error", "-y", "-i", str(outp),
                            "-f", "s16le", "-acodec", "pcm_s16le", str(dec)],
                           capture_output=True)
        if p.returncode != 0:
            row["status"] = "error"
            row["error"] = p.stderr.decode()[-300:]
        else:
            got = dec.read_bytes()
            row["in_bytes"] = len(s16) * 2
            row["decoded_bytes"] = len(got)
            row["bit_exact"] = got == inp.read_bytes()
            row["status"] = "exact" if row["bit_exact"] else "mismatch"
    else:
        row["error"] = e.get("error") or e.get("stderr")
    flac_rows.append(row)
    print(json.dumps(row), flush=True)
results["flac"] = flac_rows

# ── Malformed Opus input behavior ─────────────────────────────────────────
mal_rows = []
base_pkt = sorted((OUT / "pkts-ruopus-n4800-ch1").glob("pkt_*.bin"))
if base_pkt:
    good = base_pkt[0].read_bytes()
    variants = {
        "truncated": good[: max(1, len(good) // 2)],
        "empty": b"",
        "one-byte": good[:1],
        "garbage": bytes((i * 37) & 0xFF for i in range(64)),
        "bad-toc": b"\xff" + good[1:],
    }
    for name, blob in variants.items():
        vf = OUT / f"mal-{name}.bin"
        vf.write_bytes(blob)
        for dec in OPUS_DECS:
            d = run([BIN, "opus-dec", dec, 1, OUT / "mal-out.f32le", vf])
            st = d["status"] if d["status"] != "ok" else ("decoded" if d.get("decoded_per_ch") else "empty-ok")
            mal_rows.append({"variant": name, "dec": dec, "status": st,
                             "out": d.get("decoded_per_ch"), "err": d.get("error")})
            print(json.dumps({"mal": name, "dec": dec, "st": st,
                              "out": d.get("decoded_per_ch")}), flush=True)
results["malformed"] = mal_rows

(OUT / "audio-matrix.json").write_text(json.dumps(results, indent=2) + "\n")

# Scoreboard
print("\n═══ SCOREBOARD ═══")
for dec in OPUS_DECS:
    sub = [r for r in opus_rows if r.get("dec") == dec]
    ok = sum(1 for r in sub if r.get("count_exact"))
    print(f"opus-dec {dec:9}: {ok}/{len(sub)} count-exact")
bad = [r for r in ogg_rows if r.get("input_per_ch") != r.get("decoded_per_ch")]
print(f"ruopus ogg-rt: {len(bad)}/{len(ogg_rows)} duration mismatches")
for r in flac_rows:
    print(f"flac {r['case']}: {r['status']}")
for r in aac_rows:
    print(f"aac {r['case']}: {r['status']} snr={r.get('snr_db_vs_ffmpeg') or r.get('snr_db_via_ffmpeg')}")
