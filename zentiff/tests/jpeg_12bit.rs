//! 12-bit JPEG pages (Compression 7, BitsPerSample 12) decode through zenjpeg to their samples
//! in the native 0-4095 range, as libtiff returns them.
//!
//! The pages here are lossless JPEG (T.81 Annex H, SOF3) at 12-bit precision, written by the
//! small encoder below: zenjpeg decodes them through the same 12-bit path as DCT JPEG, and being
//! lossless they must give back exactly the samples that went in. That checks zentiff's part:
//! strips, padded tiles, shared JPEGTables and the u16 output. 12-bit DCT decoding itself is
//! checked against libjpeg-turbo in zenjpeg; against libtiff 4.7.2 with libjpeg-turbo 3.2.0,
//! 44 of 46 generated 12-bit DCT TIFFs (gray, RGB and 4:2:0 YCbCr; strips and tiles; q30/75/95)
//! decode sample-identical once libtiff's own dropped sample is set aside (it never writes the
//! last sample of a row with an odd sample count). The other two are 12-bit gray streams whose
//! IDCT overshoots far past 4095, where libjpeg-turbo wraps the value to 0 and zenjpeg clamps.
#![cfg(feature = "zenjpeg")]

use enough::Unstoppable;
use zentiff::{TiffDecodeConfig, decode};

/// Writes bits most significant first, stuffing a zero byte after each 0xFF.
struct Bits {
    out: Vec<u8>,
    acc: u32,
    n: u32,
}

impl Bits {
    fn put(&mut self, value: u32, len: u32) {
        for i in (0..len).rev() {
            self.acc = (self.acc << 1) | ((value >> i) & 1);
            self.n += 1;
            if self.n == 8 {
                let byte = self.acc as u8;
                self.out.push(byte);
                if byte == 0xFF {
                    self.out.push(0);
                }
                self.acc = 0;
                self.n = 0;
            }
        }
    }

    fn finish(mut self) -> Vec<u8> {
        while self.n != 0 {
            self.put(1, 1);
        }
        self.out
    }
}

/// The Huffman table segment: categories 0-16, each a 5-bit code equal to its category.
fn dht() -> Vec<u8> {
    let mut bits = [0u8; 16];
    bits[4] = 17;
    let mut seg = vec![0xFF, 0xC4, 0, 2 + 1 + 16 + 17, 0x00];
    seg.extend_from_slice(&bits);
    seg.extend(0..17u8);
    seg
}

/// One lossless 12-bit JPEG of `width` x `height` pixels with `samples` interleaved components
/// (predictor 1, no point transform). `with_tables` puts the Huffman table in the stream; RGB
/// streams carry an Adobe APP14 marker (transform 0) so the components read as RGB.
fn lossless_jpeg(
    width: usize,
    height: usize,
    samples: usize,
    pixels: &[u16],
    with_tables: bool,
) -> Vec<u8> {
    let mut out = vec![0xFF, 0xD8];
    if samples == 3 {
        out.extend_from_slice(&[
            0xFF, 0xEE, 0, 14, b'A', b'd', b'o', b'b', b'e', 0, 100, 0, 0, 0, 0, 0,
        ]);
    }
    if with_tables {
        out.extend(dht());
    }
    let n = samples as u8;
    out.extend_from_slice(&[0xFF, 0xC3, 0, 8 + 3 * n, 12]);
    out.extend_from_slice(&(height as u16).to_be_bytes());
    out.extend_from_slice(&(width as u16).to_be_bytes());
    out.push(n);
    for c in 0..n {
        out.extend_from_slice(&[c + 1, 0x11, 0]);
    }
    out.extend_from_slice(&[0xFF, 0xDA, 0, 6 + 2 * n, n]);
    for c in 0..n {
        out.extend_from_slice(&[c + 1, 0x00]);
    }
    out.extend_from_slice(&[1, 0, 0]);

    let mut bits = Bits {
        out: Vec::new(),
        acc: 0,
        n: 0,
    };
    let at = |x: usize, y: usize, c: usize| i32::from(pixels[(y * width + x) * samples + c]);
    for y in 0..height {
        for x in 0..width {
            for c in 0..samples {
                let prediction = match (x, y) {
                    (0, 0) => 1 << 11,
                    (_, 0) => at(x - 1, 0, c),
                    (0, _) => at(0, y - 1, c),
                    _ => at(x - 1, y, c),
                };
                let diff = at(x, y, c) - prediction;
                let category = 32 - diff.unsigned_abs().leading_zeros();
                bits.put(category, 5);
                if category > 0 {
                    let extra = if diff >= 0 { diff } else { diff - 1 };
                    bits.put(extra as u32 & ((1 << category) - 1), category);
                }
            }
        }
    }
    out.extend(bits.finish());
    out.extend_from_slice(&[0xFF, 0xD9]);
    out
}

/// A little-endian TIFF of 12-bit JPEG chunks: strips of `rows` rows, or `tile`-sized tiles,
/// with the Huffman table in each chunk or in JPEGTables.
fn tiff(
    width: usize,
    height: usize,
    samples: usize,
    pixels: &[u16],
    layout: Layout,
    shared_tables: bool,
) -> Vec<u8> {
    let (cw, ch) = match layout {
        Layout::Strips(rows) => (width, rows),
        Layout::Tiles(size) => (size, size),
    };
    let mut chunks = Vec::new();
    for ty in (0..height).step_by(ch) {
        for tx in (0..width).step_by(cw) {
            // A strip holds its own rows; a tile is coded at its full size, padded with the
            // edge samples.
            let rows = if matches!(layout, Layout::Strips(_)) {
                ch.min(height - ty)
            } else {
                ch
            };
            let mut chunk = Vec::with_capacity(cw * rows * samples);
            for y in 0..rows {
                for x in 0..cw {
                    let (sx, sy) = ((tx + x).min(width - 1), (ty + y).min(height - 1));
                    chunk.extend_from_slice(&pixels[(sy * width + sx) * samples..][..samples]);
                }
            }
            chunks.push(lossless_jpeg(cw, rows, samples, &chunk, !shared_tables));
        }
    }

    let mut out = b"II\x2a\x00\0\0\0\0".to_vec();
    let mut offsets = Vec::new();
    for c in &chunks {
        offsets.push(out.len() as u32);
        out.extend_from_slice(c);
        if out.len() % 2 == 1 {
            out.push(0);
        }
    }
    let blob = |out: &mut Vec<u8>, bytes: &[u8]| {
        let at = out.len() as u32;
        out.extend_from_slice(bytes);
        if out.len() % 2 == 1 {
            out.push(0);
        }
        at
    };
    let le32 = |v: &[u32]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
    let counts: Vec<u32> = chunks.iter().map(|c| c.len() as u32).collect();
    let offsets_at = blob(&mut out, &le32(&offsets));
    let counts_at = blob(&mut out, &le32(&counts));
    let bps_at = blob(&mut out, &[12u8, 0].repeat(samples));
    let tables = [vec![0xFF, 0xD8], dht(), vec![0xFF, 0xD9]].concat();
    let tables_at = blob(&mut out, &tables);
    let short = |v: u32| [v as u8, (v >> 8) as u8, 0, 0];
    let one = |v: u32| v.to_le_bytes();
    let n = chunks.len() as u32;
    let pointer = |count: u32, at: u32, inline: u32| if count == 1 { one(inline) } else { one(at) };
    let mut entries: Vec<(u16, u16, u32, [u8; 4])> = vec![
        (256, 4, 1, one(width as u32)),
        (257, 4, 1, one(height as u32)),
        (
            258,
            3,
            samples as u32,
            if samples == 1 { short(12) } else { one(bps_at) },
        ),
        (259, 3, 1, short(7)),
        (262, 3, 1, short(if samples == 1 { 1 } else { 2 })),
        (277, 3, 1, short(samples as u32)),
    ];
    match layout {
        Layout::Strips(rows) => entries.extend([
            (273, 4, n, pointer(n, offsets_at, offsets[0])),
            (278, 4, 1, one(rows as u32)),
            (279, 4, n, pointer(n, counts_at, counts[0])),
        ]),
        Layout::Tiles(size) => entries.extend([
            (322, 4, 1, one(size as u32)),
            (323, 4, 1, one(size as u32)),
            (324, 4, n, pointer(n, offsets_at, offsets[0])),
            (325, 4, n, pointer(n, counts_at, counts[0])),
        ]),
    }
    if shared_tables {
        entries.push((347, 7, tables.len() as u32, one(tables_at)));
    }
    entries.sort_by_key(|e| e.0);
    let ifd = out.len() as u32;
    out[4..8].copy_from_slice(&ifd.to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    for (tag, typ, count, value) in entries {
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(&typ.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&value);
    }
    out.extend_from_slice(&0u32.to_le_bytes());
    out
}

#[derive(Clone, Copy, Debug)]
enum Layout {
    Strips(usize),
    Tiles(usize),
}

#[test]
fn twelve_bit_jpeg_pages_decode_to_native_samples() {
    let (width, height) = (37usize, 23usize);
    for samples in [1usize, 3] {
        let pixels: Vec<u16> = (0..width * height * samples)
            .map(|i| {
                let (p, c) = (i / samples, i % samples);
                let (x, y) = (p % width, p / width);
                let v = (x * 4095 / (width - 1) + y * 977 + c * 1300 + (x * y) % 211) % 4096;
                // Sharp edges as well as gradients.
                (if (x / 5 + y / 3) % 4 == 0 {
                    4095 - v
                } else {
                    v
                }) as u16
            })
            .collect();
        for layout in [Layout::Strips(8), Layout::Strips(23), Layout::Tiles(16)] {
            for shared_tables in [false, true] {
                let file = tiff(width, height, samples, &pixels, layout, shared_tables);
                let out =
                    decode(&file, &TiffDecodeConfig::default(), &Unstoppable).unwrap_or_else(|e| {
                        panic!("{samples} samples, {layout:?}, shared tables {shared_tables}: {e}")
                    });
                assert_eq!(
                    (out.pixels.width(), out.pixels.height()),
                    (width as u32, height as u32)
                );
                let bytes = out.pixels.copy_to_contiguous_bytes();
                let decoded: Vec<u16> = bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|&b| u16::from_ne_bytes(b))
                    .collect();
                assert_eq!(
                    decoded, pixels,
                    "{samples} samples, {layout:?}, shared tables {shared_tables}"
                );
            }
        }
    }
}
