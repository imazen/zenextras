//! Decode every file in the tiff-conformance corpus and report results.

use enough::Unstoppable;
use zenpixels::PixelDescriptor;
use zentiff::{TiffDecodeConfig, decode, probe};

fn try_all_files_in(dir: &std::path::Path, results: &mut Vec<(String, Result<String, String>)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<_> = entries.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            try_all_files_in(&path, results);
            continue;
        }
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ext != "tif" && ext != "tiff" {
            continue;
        }
        let name = path
            .strip_prefix(dir.ancestors().nth(2).unwrap_or(dir))
            .unwrap_or(&path)
            .display()
            .to_string();

        let data = match std::fs::read(&path) {
            Ok(d) => d,
            Err(e) => {
                results.push((name, Err(format!("read error: {e}"))));
                continue;
            }
        };

        // Try probe first
        let probe_result = probe(&data);

        // Try full decode
        let config = TiffDecodeConfig::none();
        match decode(&data, &config, &Unstoppable) {
            Ok(output) => {
                let desc = output.pixels.descriptor();
                let info = format!(
                    "{}x{} {:?}/{:?} ch={} bd={}",
                    output.info.width,
                    output.info.height,
                    desc.layout(),
                    desc.channel_type(),
                    output.info.channels,
                    output.info.bit_depth,
                );
                results.push((name, Ok(info)));
            }
            Err(e) => {
                let probe_info = match probe_result {
                    Ok(info) => format!(
                        " (probe: {}x{} ch={} bd={})",
                        info.width, info.height, info.channels, info.bit_depth
                    ),
                    Err(_) => String::new(),
                };
                results.push((name, Err(format!("{e}{probe_info}"))));
            }
        }
    }
}

/// Diagnose buffer-size failures by decoding manually and comparing sizes.
fn diagnose_buffer_failures(dir: &std::path::Path) {
    use std::io::Cursor;
    use tiff::decoder::Decoder;

    eprintln!("\n=== BUFFER SIZE DIAGNOSIS ===");
    eprintln!(
        "{:<55} {:>10} {:>10} {:>10} {:>6} {:>5} {:>8}",
        "FILE", "DECODED", "EXPECTED", "DIFF", "STRIDE", "ALIGN", "OFFSET"
    );

    let mut paths = Vec::new();
    collect_tiffs(dir, &mut paths);
    paths.sort();

    for path in &paths {
        let data = std::fs::read(path).unwrap();
        let cursor = Cursor::new(&data);
        let mut dec = match Decoder::new(cursor) {
            Ok(d) => d,
            Err(_) => continue,
        };
        let (w, h) = match dec.dimensions() {
            Ok(d) => d,
            Err(_) => continue,
        };
        let ct = match dec.colortype() {
            Ok(c) => c,
            Err(_) => continue,
        };

        let result = match dec.read_image() {
            Ok(r) => r,
            Err(_) => continue,
        };

        let decoded_bytes = decoding_result_byte_len(&result);

        // Replicate zentiff's descriptor selection
        let is_float = matches!(
            &result,
            tiff::decoder::DecodingResult::F16(_)
                | tiff::decoder::DecodingResult::F32(_)
                | tiff::decoder::DecodingResult::F64(_)
        );
        let descriptor = descriptor_for_diag(ct, is_float);
        let stride = descriptor.aligned_stride(w);
        let expected = stride * h as usize;
        let align = descriptor.min_alignment();

        // Simulate align_offset with a hypothetical vec
        let test_vec: Vec<u8> = vec![0u8; decoded_bytes];
        let ptr = test_vec.as_ptr();
        let addr = ptr as usize;
        let aligned_addr = (addr + align - 1) & !(align - 1);
        let offset = aligned_addr - addr;

        if decoded_bytes < offset + expected {
            let name = path.file_name().unwrap().to_str().unwrap();
            let diff = (offset + expected) as i64 - decoded_bytes as i64;
            eprintln!(
                "{:<55} {:>10} {:>10} {:>+10} {:>6} {:>5} {:>8}",
                name, decoded_bytes, expected, diff, stride, align, offset
            );

            // Also show what the tiff crate thinks the image is
            let (samples, variant) = decoding_result_info(&result);
            eprintln!(
                "  color_type={:?} decoded_as={} samples={} w*h*bpp={}",
                ct,
                variant,
                samples,
                w as usize * h as usize * descriptor.bytes_per_pixel()
            );
        }
    }
}

fn collect_tiffs(dir: &std::path::Path, paths: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            collect_tiffs(&p, paths);
        } else {
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
            if ext == "tif" || ext == "tiff" {
                paths.push(p);
            }
        }
    }
}

fn decoding_result_byte_len(r: &tiff::decoder::DecodingResult) -> usize {
    use tiff::decoder::DecodingResult as DR;
    match r {
        DR::U8(v) => v.len(),
        DR::U16(v) => v.len() * 2,
        DR::U32(v) => v.len() * 4,
        DR::U64(v) => v.len() * 8,
        DR::I8(v) => v.len(),
        DR::I16(v) => v.len() * 2,
        DR::I32(v) => v.len() * 4,
        DR::I64(v) => v.len() * 8,
        DR::F16(v) => v.len() * 2,
        DR::F32(v) => v.len() * 4,
        DR::F64(v) => v.len() * 8,
    }
}

fn decoding_result_info(r: &tiff::decoder::DecodingResult) -> (usize, &'static str) {
    use tiff::decoder::DecodingResult as DR;
    match r {
        DR::U8(v) => (v.len(), "U8"),
        DR::U16(v) => (v.len(), "U16"),
        DR::U32(v) => (v.len(), "U32"),
        DR::U64(v) => (v.len(), "U64"),
        DR::I8(v) => (v.len(), "I8"),
        DR::I16(v) => (v.len(), "I16"),
        DR::I32(v) => (v.len(), "I32"),
        DR::I64(v) => (v.len(), "I64"),
        DR::F16(v) => (v.len(), "F16"),
        DR::F32(v) => (v.len(), "F32"),
        DR::F64(v) => (v.len(), "F64"),
    }
}

/// Replicate zentiff's descriptor_for logic for diagnosis.
fn descriptor_for_diag(ct: tiff::ColorType, is_float: bool) -> PixelDescriptor {
    match ct {
        tiff::ColorType::Gray(d) => match d {
            1..=8 => PixelDescriptor::GRAY8,
            9..=16 if is_float => PixelDescriptor::GRAYF32,
            9..=16 => PixelDescriptor::GRAY16,
            _ if is_float => PixelDescriptor::GRAYF32,
            _ => PixelDescriptor::GRAY16,
        },
        tiff::ColorType::GrayA(d) => match d {
            1..=8 => PixelDescriptor::GRAYA8,
            9..=16 if is_float => PixelDescriptor::GRAYAF32,
            9..=16 => PixelDescriptor::GRAYA16,
            _ if is_float => PixelDescriptor::GRAYAF32,
            _ => PixelDescriptor::GRAYA16,
        },
        tiff::ColorType::RGB(d) | tiff::ColorType::YCbCr(d) | tiff::ColorType::Lab(d) => match d {
            1..=8 => PixelDescriptor::RGB8,
            9..=16 if is_float => PixelDescriptor::RGBF32,
            9..=16 => PixelDescriptor::RGB16,
            _ if is_float => PixelDescriptor::RGBF32,
            _ => PixelDescriptor::RGB16,
        },
        tiff::ColorType::RGBA(d) => match d {
            1..=8 => PixelDescriptor::RGBA8,
            9..=16 if is_float => PixelDescriptor::RGBAF32,
            9..=16 => PixelDescriptor::RGBA16,
            _ if is_float => PixelDescriptor::RGBAF32,
            _ => PixelDescriptor::RGBA16,
        },
        tiff::ColorType::Palette(_) => PixelDescriptor::RGB8,
        tiff::ColorType::CMYK(d) | tiff::ColorType::CMYKA(d) => match d {
            1..=8 => PixelDescriptor::RGBA8,
            9..=16 if is_float => PixelDescriptor::RGBAF32,
            9..=16 => PixelDescriptor::RGBA16,
            _ if is_float => PixelDescriptor::RGBAF32,
            _ => PixelDescriptor::RGBA16,
        },
        tiff::ColorType::Multiband {
            bit_depth,
            num_samples,
        } => match (num_samples, bit_depth) {
            (1, 1..=8) => PixelDescriptor::GRAY8,
            (1, _) => PixelDescriptor::GRAY16,
            (3, 1..=8) => PixelDescriptor::RGB8,
            (3, _) => PixelDescriptor::RGB16,
            (4, 1..=8) => PixelDescriptor::RGBA8,
            (4, _) => PixelDescriptor::RGBA16,
            (_, 1..=8) => PixelDescriptor::RGBA8,
            _ => PixelDescriptor::RGBA16,
        },
        _ => PixelDescriptor::RGBA8,
    }
}

#[test]
fn corpus_decode_all() {
    let corpus = match codec_corpus::Corpus::new() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("codec-corpus unavailable ({e}), skipping");
            return;
        }
    };
    let tiff_dir = match corpus.get("tiff-conformance") {
        Ok(d) => d.to_path_buf(),
        Err(e) => {
            eprintln!("tiff-conformance not available ({e}), skipping");
            return;
        }
    };

    let mut results = Vec::new();
    try_all_files_in(&tiff_dir, &mut results);

    let mut ok_count = 0;
    let mut fail_count = 0;

    eprintln!("\n=== DECODE FAILURES ===");
    for (name, result) in &results {
        match result {
            Ok(_) => ok_count += 1,
            Err(e) => {
                fail_count += 1;
                eprintln!("FAIL: {name}");
                eprintln!("      {e}");
            }
        }
    }

    eprintln!("\n=== DECODE SUCCESSES ===");
    for (name, result) in &results {
        if let Ok(info) = result {
            eprintln!("  OK: {name} -> {info}");
        }
    }

    eprintln!("\n=== SUMMARY ===");
    eprintln!("OK:   {ok_count}");
    eprintln!("FAIL: {fail_count}");
    eprintln!("TOTAL: {}", ok_count + fail_count);

    // Run buffer size diagnosis
    diagnose_buffer_failures(&tiff_dir);
}
