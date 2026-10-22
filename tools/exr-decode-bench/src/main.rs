//! Decode-speed comparison of EXR compressions (B44, B44A, DWAA, DWAB, with
//! NONE/ZIP/PIZ as lossless references) between zenexr / the `exr` crate and
//! the OpenEXR C++ library. See README.md for the method.
//!
//! Commands:
//!   prepare <src-dir> <out-dir> <openexr-bench>
//!   verify <out-dir> <openexr-bench> <verify.csv>
//!   bench <threads> <min-ms> <min-samples> <max-samples> <file>...
//!   run <out-dir> <openexr-bench> <threads> <rounds> <min-ms> <results.csv> [filter]

use enough::Unstoppable;
use exr::block::UncompressedBlock;
use exr::block::chunk::{Chunk, CompressedBlock};
use exr::prelude::*;
use rayon::prelude::*;
use std::fmt::Write as _;
use std::io::{Cursor, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

type BoxResult<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Crop edge lengths; every source also contributes its full frame.
const CROPS: [usize; 3] = [64, 256, 1024];

/// (encoder, compression, dwa level). Lossless references and non-default DWA
/// levels come from the OpenEXR encoder only; the default-level lossy files
/// come from both encoders so bitstream differences show up separately.
const ENCODINGS: &[(&str, &str, Option<u32>)] = &[
    ("openexr", "none", None),
    ("openexr", "zip", None),
    ("openexr", "piz", None),
    ("openexr", "b44", None),
    ("openexr", "b44a", None),
    ("openexr", "dwaa", Some(15)),
    ("openexr", "dwaa", Some(45)),
    ("openexr", "dwaa", Some(150)),
    ("openexr", "dwab", Some(15)),
    ("openexr", "dwab", Some(45)),
    ("openexr", "dwab", Some(150)),
    ("exr", "b44", None),
    ("exr", "b44a", None),
    ("exr", "dwaa", Some(45)),
    ("exr", "dwab", Some(45)),
];

fn main() -> BoxResult<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("prepare") if args.len() == 4 => prepare(
            Path::new(&args[1]),
            Path::new(&args[2]),
            Path::new(&args[3]),
        ),
        Some("verify") if args.len() == 4 => verify(
            Path::new(&args[1]),
            Path::new(&args[2]),
            Path::new(&args[3]),
        ),
        Some("bench") if args.len() >= 6 => bench(
            args[1].parse()?,
            args[2].parse()?,
            args[3].parse()?,
            args[4].parse()?,
            &args[5..],
        ),
        Some("run") if args.len() >= 7 => run(
            Path::new(&args[1]),
            Path::new(&args[2]),
            args[3].parse()?,
            args[4].parse()?,
            args[5].parse()?,
            Path::new(&args[6]),
            args.get(7).map(String::as_str),
        ),
        _ => Err("usage: prepare|verify|bench|run (see src/main.rs)".into()),
    }
}

// ---------------------------------------------------------------- prepare

struct Rgb16 {
    w: usize,
    h: usize,
    /// Interleaved RGB half samples.
    px: Vec<f16>,
}

fn read_source(path: &Path) -> BoxResult<Rgb16> {
    let image = read()
        .no_deep_data()
        .largest_resolution_level()
        .rgb_channels(
            |size, _| Rgb16 {
                w: size.width(),
                h: size.height(),
                px: vec![f16::ZERO; size.area() * 3],
            },
            |img: &mut Rgb16, pos, (r, g, b): (f16, f16, f16)| {
                let i = (pos.y() * img.w + pos.x()) * 3;
                img.px[i..i + 3].copy_from_slice(&[r, g, b]);
            },
        )
        .first_valid_layer()
        .all_attributes()
        .from_file(path)?;
    Ok(image.layer_data.channel_data.pixels)
}

fn crop(src: &Rgb16, w: usize, h: usize) -> Rgb16 {
    let (x0, y0) = ((src.w - w) / 2, (src.h - h) / 2);
    let mut px = Vec::with_capacity(w * h * 3);
    for y in y0..y0 + h {
        let row = (y * src.w + x0) * 3;
        px.extend_from_slice(&src.px[row..row + w * 3]);
    }
    Rgb16 { w, h, px }
}

fn exr_compression(name: &str, level: Option<u32>) -> Compression {
    let level = level.map(|l| l as f32);
    match name {
        "none" => Compression::Uncompressed,
        "zip" => Compression::ZIP16,
        "piz" => Compression::PIZ,
        "b44" => Compression::B44,
        "b44a" => Compression::B44A,
        "dwaa" => Compression::DWAA(level),
        "dwab" => Compression::DWAB(level),
        _ => panic!("unknown compression {name}"),
    }
}

fn write_exr(img: &Rgb16, compression: Compression, path: &Path, parallel: bool) -> BoxResult<()> {
    let w = img.w;
    let pixels = SpecificChannels::rgb(|pos: Vec2<usize>| {
        let i = (pos.y() * w + pos.x()) * 3;
        (img.px[i], img.px[i + 1], img.px[i + 2])
    });
    let encoding = Encoding {
        compression,
        blocks: Blocks::ScanLines,
        line_order: LineOrder::Increasing,
    };
    let image = Image::from_encoded_channels((img.w, img.h), encoding, pixels);
    let writer = image.write();
    let writer = if parallel {
        writer
    } else {
        writer.non_parallel()
    };
    writer.to_file(path)?;
    Ok(())
}

fn file_name(encoder: &str, compression: &str, level: Option<u32>) -> String {
    match level {
        Some(l) => format!("{encoder}_{compression}_{l}.exr"),
        None => format!("{encoder}_{compression}.exr"),
    }
}

fn prepare(src_dir: &Path, out_dir: &Path, openexr_bench: &Path) -> BoxResult<()> {
    let mut sources: Vec<PathBuf> = std::fs::read_dir(src_dir)?
        .map(|e| e.map(|e| e.path()))
        .collect::<std::io::Result<_>>()?;
    sources.retain(|p| p.extension().is_some_and(|e| e == "exr"));
    sources.sort();
    for source in sources {
        let stem = source.file_stem().unwrap().to_string_lossy().into_owned();
        let full = read_source(&source)?;
        let mut variants: Vec<(usize, usize)> = CROPS
            .iter()
            .filter(|&&s| s <= full.w && s <= full.h)
            .map(|&s| (s, s))
            .collect();
        if !variants.contains(&(full.w, full.h)) {
            variants.push((full.w, full.h));
        }
        for (w, h) in variants {
            let dir = out_dir.join(&stem).join(format!("{w}x{h}"));
            std::fs::create_dir_all(&dir)?;
            let img = crop(&full, w, h);
            let source_path = dir.join("source.exr");
            write_exr(&img, Compression::Uncompressed, &source_path, false)?;
            for &(encoder, compression, level) in ENCODINGS {
                let out = dir.join(file_name(encoder, compression, level));
                if encoder == "exr" {
                    // Sequential so an encoder panic unwinds here instead of
                    // aborting inside rayon; failures are recorded, not fatal.
                    let c = exr_compression(compression, level);
                    match std::panic::catch_unwind(|| write_exr(&img, c, &out, false)) {
                        Ok(r) => r?,
                        Err(panic) => {
                            let msg = panic
                                .downcast_ref::<String>()
                                .cloned()
                                .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
                                .unwrap_or_default();
                            std::fs::remove_file(&out).ok();
                            let mut log = std::fs::OpenOptions::new()
                                .create(true)
                                .append(true)
                                .open(out_dir.join("encode_failures.txt"))?;
                            writeln!(log, "{}\tpanic: {msg}", out.display())?;
                        }
                    }
                } else {
                    let mut cmd = Command::new(openexr_bench);
                    cmd.arg("encode")
                        .arg(&source_path)
                        .arg(&out)
                        .arg(compression);
                    if let Some(l) = level {
                        cmd.arg(l.to_string());
                    }
                    let status = cmd.status()?;
                    if !status.success() {
                        return Err(format!("openexr encode failed: {}", out.display()).into());
                    }
                }
            }
            eprintln!("prepared {}", dir.display());
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- decoders

/// Planar R, G, B half bits.
struct Planes {
    data: Vec<u16>,
}

fn plane_index(name: &Text) -> Option<usize> {
    ["R", "G", "B"].iter().position(|n| name.eq(n))
}

/// High-level `exr` read of every channel as flat samples.
fn exr_f16_planar(bytes: &[u8], parallel: bool) -> BoxResult<Planes> {
    let reader = read()
        .no_deep_data()
        .largest_resolution_level()
        .all_channels()
        .first_valid_layer()
        .all_attributes();
    let reader = if parallel {
        reader
    } else {
        reader.non_parallel()
    };
    let image = reader.from_buffered(Cursor::new(bytes))?;
    let layer = image.layer_data;
    let (w, h) = (layer.size.width(), layer.size.height());
    // Reordering B,G,R storage into R,G,B planes keeps the output identical to
    // the C++ arm; it is a copy both sides would otherwise not need.
    let mut data = vec![0u16; w * h * 3];
    for channel in &layer.channel_data.list {
        let Some(p) = plane_index(&channel.name) else {
            continue;
        };
        let FlatSamples::F16(samples) = &channel.sample_data else {
            return Err("expected half samples".into());
        };
        for (dst, src) in data[p * w * h..(p + 1) * w * h].iter_mut().zip(samples) {
            *dst = src.to_bits();
        }
    }
    Ok(Planes { data })
}

/// Block-level `exr` decode: decompress every chunk and copy its lines into
/// R, G, B planes. With a pool, chunks decompress and copy on that persistent
/// pool, which isolates codec throughput from `exr`'s per-call pool creation.
fn exr_blocks_f16_planar(bytes: &[u8], pool: Option<&rayon::ThreadPool>) -> BoxResult<Planes> {
    let reader = exr::block::read(Cursor::new(bytes), false)?;
    let meta = reader.meta_data().clone();
    let header = &meta.headers[0];
    let (w, h) = (header.layer_size.width(), header.layer_size.height());
    let lines = header.compression.scan_lines_per_block();
    let y0 = header.own_attributes.layer_position.y();
    let channel_planes: Vec<Option<usize>> = header
        .channels
        .list
        .iter()
        .map(|c| plane_index(&c.name))
        .collect();
    let mut chunks: Vec<Chunk> = reader
        .all_chunks(false)?
        .collect::<exr::error::Result<_>>()?;
    chunks.sort_by_key(|c| match &c.compressed_block {
        CompressedBlock::ScanLine(b) => b.y_coordinate,
        _ => i32::MAX,
    });
    let mut data = vec![0u16; w * h * 3];
    let (r, rest) = data.split_at_mut(w * h);
    let (g, b) = rest.split_at_mut(w * h);
    let mut jobs: Vec<(Chunk, [&mut [u16]; 3])> = chunks
        .into_iter()
        .zip(
            r.chunks_mut(w * lines)
                .zip(g.chunks_mut(w * lines))
                .zip(b.chunks_mut(w * lines)),
        )
        .map(|(c, ((r, g), b))| (c, [r, g, b]))
        .collect();
    let decode = |(chunk, planes): &mut (Chunk, [&mut [u16]; 3])| -> exr::error::Result<()> {
        let expected_y = match &chunk.compressed_block {
            CompressedBlock::ScanLine(b) => b.y_coordinate - y0,
            _ => return Err(exr::error::Error::NotSupported("tiles".into())),
        };
        let block = UncompressedBlock::decompress_chunk(chunk.clone(), &meta, false)?;
        debug_assert_eq!(block.index.pixel_position.y() as i32, expected_y);
        let block_lines = block.index.pixel_size.height();
        let mut src = block.data.as_chunks::<2>().0.iter();
        for line in 0..block_lines {
            for plane in &channel_planes {
                let row = src.by_ref().take(w);
                match plane {
                    Some(p) => {
                        for (dst, s) in planes[*p][line * w..(line + 1) * w].iter_mut().zip(row) {
                            *dst = u16::from_ne_bytes(*s);
                        }
                    }
                    None => row.for_each(drop),
                }
            }
        }
        Ok(())
    };
    match pool {
        Some(pool) => pool.install(|| jobs.par_iter_mut().try_for_each(decode))?,
        None => jobs.iter_mut().try_for_each(decode)?,
    }
    Ok(Planes { data })
}

/// High-level `exr` read into interleaved RGB f32 (the C++ f32 arm's layout).
fn exr_f32_rgb(bytes: &[u8], parallel: bool) -> BoxResult<Vec<f32>> {
    let reader = read()
        .no_deep_data()
        .largest_resolution_level()
        .rgb_channels(
            |size, _| (size.width(), vec![0f32; size.area() * 3]),
            |(w, px): &mut (usize, Vec<f32>), pos, (r, g, b): (f32, f32, f32)| {
                let i = (pos.y() * *w + pos.x()) * 3;
                px[i..i + 3].copy_from_slice(&[r, g, b]);
            },
        )
        .first_valid_layer()
        .all_attributes();
    let reader = if parallel {
        reader
    } else {
        reader.non_parallel()
    };
    Ok(reader
        .from_buffered(Cursor::new(bytes))?
        .layer_data
        .channel_data
        .pixels
        .1)
}

/// zenexr as shipped: single-threaded, RGB(A) f32 `PixelBuffer`.
fn zenexr_f32_rgb(bytes: &[u8]) -> BoxResult<Vec<f32>> {
    let image = zenexr::ExrDecoderConfig::new().decode(bytes, &Unstoppable)?;
    let pixels = image.pixels();
    let view = pixels.as_slice();
    let mut out = Vec::with_capacity(pixels.width() as usize * pixels.height() as usize * 3);
    for y in 0..pixels.height() {
        out.extend(
            view.row(y)
                .as_chunks::<4>()
                .0
                .iter()
                .map(|b| f32::from_ne_bytes(*b)),
        );
    }
    Ok(out)
}

// ---------------------------------------------------------------- verify

fn cpp_dump(openexr_bench: &Path, file: &Path, mode: &str, tmp: &Path) -> BoxResult<Vec<u8>> {
    let status = Command::new(openexr_bench)
        .arg("dump")
        .arg(file)
        .arg(tmp)
        .arg(mode)
        .status()?;
    if !status.success() {
        return Err(format!("openexr dump failed: {}", file.display()).into());
    }
    Ok(std::fs::read(tmp)?)
}

fn encoded_files(out_dir: &Path, filter: Option<&str>) -> BoxResult<Vec<PathBuf>> {
    let mut files = Vec::new();
    for image in std::fs::read_dir(out_dir)? {
        let image = image?.path();
        if !image.is_dir() {
            continue;
        }
        for size in std::fs::read_dir(&image)? {
            for f in std::fs::read_dir(size?.path())? {
                let f = f?.path();
                let name = f.file_name().unwrap().to_string_lossy().into_owned();
                if name.ends_with(".exr")
                    && name != "source.exr"
                    && filter.is_none_or(|s| f.to_string_lossy().contains(s))
                {
                    files.push(f);
                }
            }
        }
    }
    files.sort();
    Ok(files)
}

fn count_diff_u16(a: &[u16], b: &[u16]) -> usize {
    if a.len() != b.len() {
        return usize::MAX;
    }
    a.iter().zip(b).filter(|(x, y)| x != y).count()
}

fn count_diff_f32(a: &[f32], b: &[f32]) -> usize {
    if a.len() != b.len() {
        return usize::MAX;
    }
    a.iter()
        .zip(b)
        .filter(|(x, y)| x.to_bits() != y.to_bits())
        .count()
}

fn planar_from_source(path: &Path) -> BoxResult<Vec<u16>> {
    let src = read_source(path)?;
    let n = src.w * src.h;
    let mut out = vec![0u16; n * 3];
    for (i, px) in src.px.chunks_exact(3).enumerate() {
        for c in 0..3 {
            out[c * n + i] = px[c].to_bits();
        }
    }
    Ok(out)
}

fn verify(out_dir: &Path, openexr_bench: &Path, csv: &Path) -> BoxResult<()> {
    let tmp = out_dir.join(".verify.raw");
    let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build()?;
    let mut report = String::from(
        "file,samples,cpp_vs_exr_f16,cpp_vs_exr_f16_par,cpp_vs_exr_blocks,cpp_vs_exr_blocks_pool,\
         cpp_f32_vs_exr_f32,cpp_f32_vs_zenexr,cpp_vs_source\n",
    );
    let mut failures = 0;
    for file in encoded_files(out_dir, None)? {
        let bytes = std::fs::read(&file)?;
        let cpp: Vec<u16> = cpp_dump(openexr_bench, &file, "half", &tmp)?
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| u16::from_le_bytes(*b))
            .collect();
        let cpp_f32: Vec<f32> = cpp_dump(openexr_bench, &file, "f32rgb", &tmp)?
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect();
        let source = planar_from_source(&file.with_file_name("source.exr"))?;
        let diffs = [
            count_diff_u16(&cpp, &exr_f16_planar(&bytes, false)?.data),
            count_diff_u16(&cpp, &exr_f16_planar(&bytes, true)?.data),
            count_diff_u16(&cpp, &exr_blocks_f16_planar(&bytes, None)?.data),
            count_diff_u16(&cpp, &exr_blocks_f16_planar(&bytes, Some(&pool))?.data),
            count_diff_f32(&cpp_f32, &exr_f32_rgb(&bytes, false)?),
            count_diff_f32(&cpp_f32, &zenexr_f32_rgb(&bytes)?),
        ];
        let vs_source = count_diff_u16(&cpp, &source);
        if diffs.iter().any(|&d| d != 0) {
            failures += 1;
            eprintln!("MISMATCH {} {diffs:?}", file.display());
        }
        let name = file.file_name().unwrap().to_string_lossy();
        if (name.contains("_none") || name.contains("_zip") || name.contains("_piz"))
            && vs_source != 0
        {
            failures += 1;
            eprintln!("LOSSLESS MISMATCH {} {vs_source}", file.display());
        }
        let _ = writeln!(
            report,
            "{},{},{},{},{},{},{},{},{}",
            file.display(),
            cpp.len(),
            diffs[0],
            diffs[1],
            diffs[2],
            diffs[3],
            diffs[4],
            diffs[5],
            vs_source
        );
    }
    std::fs::remove_file(&tmp).ok();
    std::fs::write(csv, report)?;
    eprintln!(
        "verify: {failures} files with mismatches; table in {}",
        csv.display()
    );
    Ok(())
}

// ---------------------------------------------------------------- bench

fn percentile(sorted: &[u64], q: f64) -> u64 {
    sorted[(q * (sorted.len() - 1) as f64 + 0.5) as usize]
}

type ArmFn<'a> = Box<dyn Fn(&[u8]) -> BoxResult<u64> + 'a>;

/// Same timing protocol as the C++ side: one warm-up call per arm, then
/// rounds in shuffled arm order until every arm has `min_samples` and either
/// `min_ms` of total time or `max_samples`.
fn bench(
    threads: usize,
    min_ms: u64,
    min_samples: usize,
    max_samples: usize,
    files: &[String],
) -> BoxResult<()> {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()?;
    let mut arms: Vec<(&str, ArmFn)> = Vec::new();
    if threads <= 1 {
        arms.push((
            "zenexr-f32-rgb",
            Box::new(|b| Ok(zenexr_f32_rgb(b)?[0].to_bits() as u64)),
        ));
        arms.push((
            "exr-f32-rgb",
            Box::new(|b| Ok(exr_f32_rgb(b, false)?[0].to_bits() as u64)),
        ));
        arms.push((
            "exr-f16-planar",
            Box::new(|b| Ok(exr_f16_planar(b, false)?.data[0] as u64)),
        ));
        arms.push((
            "exr-blocks-f16-planar",
            Box::new(|b| Ok(exr_blocks_f16_planar(b, None)?.data[0] as u64)),
        ));
    } else {
        // `exr`'s parallel reader builds a rayon pool per call; its size
        // follows RAYON_NUM_THREADS, which the driver sets to `threads`.
        arms.push((
            "exr-f32-rgb",
            Box::new(|b| Ok(exr_f32_rgb(b, true)?[0].to_bits() as u64)),
        ));
        arms.push((
            "exr-f16-planar",
            Box::new(|b| Ok(exr_f16_planar(b, true)?.data[0] as u64)),
        ));
        arms.push((
            "exr-blocks-f16-planar-pool",
            Box::new(|b| Ok(exr_blocks_f16_planar(b, Some(&pool))?.data[0] as u64)),
        ));
    }
    let min_ns = min_ms * 1_000_000;
    let mut rng: u64 = 0x9e37_79b9_7f4a_7c15;
    let stdout = std::io::stdout();
    for file in files {
        let bytes = std::fs::read(file)?;
        let mut samples: Vec<Vec<u64>> = vec![Vec::new(); arms.len()];
        let mut totals = vec![0u64; arms.len()];
        let mut checksums = vec![0u64; arms.len()];
        for (i, (_, f)) in arms.iter().enumerate() {
            checksums[i] = f(&bytes)?;
        }
        let mut order: Vec<usize> = (0..arms.len()).collect();
        while !samples
            .iter()
            .zip(&totals)
            .all(|(s, &t)| s.len() >= min_samples && (t >= min_ns || s.len() >= max_samples))
        {
            for i in (1..order.len()).rev() {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                order.swap(i, (rng % (i as u64 + 1)) as usize);
            }
            for &a in &order {
                let t0 = Instant::now();
                let c = std::hint::black_box((arms[a].1)(&bytes)?);
                let ns = t0.elapsed().as_nanos() as u64;
                samples[a].push(ns);
                totals[a] += ns;
                checksums[a] ^= c;
            }
        }
        let mut out = stdout.lock();
        for (i, (name, _)) in arms.iter().enumerate() {
            let s = &mut samples[i];
            s.sort_unstable();
            writeln!(
                out,
                "{file},{threads},{name},{},{},{},{},{},{}",
                s.len(),
                percentile(s, 0.5),
                percentile(s, 0.25),
                percentile(s, 0.75),
                s[0],
                checksums[i]
            )?;
        }
        out.flush()?;
    }
    Ok(())
}

// ---------------------------------------------------------------- run

/// Alternates the C++ and Rust processes per file (order flips every round),
/// so the two sides measure each file close together in time.
fn run(
    out_dir: &Path,
    openexr_bench: &Path,
    threads: usize,
    rounds: usize,
    min_ms: u64,
    csv: &Path,
    filter: Option<&str>,
) -> BoxResult<()> {
    let me = std::env::current_exe()?;
    let files = encoded_files(out_dir, filter)?;
    let mut out = std::fs::File::create(csv)?;
    writeln!(
        out,
        "unix_ms,image,width,height,encoder,compression,level,file_bytes,bits_per_pixel,round,threads,\
         arm,samples,median_ns,p25_ns,p75_ns,min_ns,mpix_per_s"
    )?;
    let started = Instant::now();
    for (index, file) in files.iter().enumerate() {
        let size_dir = file.parent().unwrap();
        let image = size_dir
            .parent()
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy();
        let (w, h) = size_dir
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .split_once('x')
            .unwrap();
        let (w, h): (u64, u64) = (w.parse()?, h.parse()?);
        let stem = file.file_stem().unwrap().to_string_lossy();
        let mut parts = stem.split('_');
        let encoder = parts.next().unwrap();
        let compression = parts.next().unwrap();
        let level = parts.next().unwrap_or("");
        let bytes = std::fs::metadata(file)?.len();
        let bpp = bytes as f64 * 8.0 / (w * h) as f64;
        for round in 0..rounds {
            let cpp_first = (index + round) % 2 == 0;
            for side in 0..2 {
                let cpp = (side == 0) == cpp_first;
                let mut cmd = if cpp {
                    Command::new(openexr_bench)
                } else {
                    Command::new(&me)
                };
                cmd.env("RAYON_NUM_THREADS", threads.to_string()).args([
                    "bench",
                    &threads.to_string(),
                    &min_ms.to_string(),
                    "5",
                    "5000",
                ]);
                // Start time lets rows be matched against a contention log.
                let unix_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)?
                    .as_millis();
                let output = cmd.arg(file).output()?;
                if !output.status.success() {
                    return Err(format!(
                        "bench failed on {}: {}",
                        file.display(),
                        String::from_utf8_lossy(&output.stderr)
                    )
                    .into());
                }
                for line in String::from_utf8(output.stdout)?.lines() {
                    let f: Vec<&str> = line.split(',').collect();
                    let median: f64 = f[4].parse()?;
                    writeln!(
                        out,
                        "{unix_ms},{image},{w},{h},{encoder},{compression},{level},{bytes},{bpp:.4},\
                         {round},{},{},{},{},{},{},{},{:.3}",
                        f[1],
                        f[2],
                        f[3],
                        f[4],
                        f[5],
                        f[6],
                        f[7],
                        (w * h) as f64 / median * 1000.0
                    )?;
                }
            }
        }
        out.flush()?;
        if index % 25 == 0 {
            eprintln!(
                "[{:>6.0}s] {}/{} {}",
                started.elapsed().as_secs_f64(),
                index + 1,
                files.len(),
                file.display()
            );
        }
    }
    Ok(())
}
