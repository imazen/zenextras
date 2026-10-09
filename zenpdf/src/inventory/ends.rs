//! Where a stream's first filter stops reading its raw bytes, so bytes after
//! that internal end are not reported as consumed. Each rule mirrors the
//! hayro-syntax decoder the renderer uses (`filter/*.rs` at
//! `lilith/hayro@beec7225`).

/// The outcome of looking for a stream's internal end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum End {
    /// The decoder stops reading at this offset into the stream data.
    At(usize),
    /// The decoder reads every byte (or the end could not be located, and
    /// the decoder then reads to the end of the data).
    Whole,
    /// Bytes after the internal end are not distinguished for this data.
    Unchecked(&'static str),
}

/// hayro's `MAX_DECODED_STREAM_BYTES`: a stream that inflates past it is
/// rejected.
const MAX_DECODED: u64 = 512 << 20;

/// The internal end of `data` under `filter` (the first filter in the
/// stream's `/Filter`, without its slash), or of unfiltered data with
/// `image` geometry when given.
pub(crate) fn filter_end(filter: Option<&[u8]>, data: &[u8], image: Option<ImageGeometry>) -> End {
    match filter {
        Some(b"FlateDecode" | b"Fl") => flate_end(data),
        Some(b"ASCIIHexDecode" | b"AHx") => match data.iter().position(|&b| b == b'>') {
            Some(i) => End::At(i + 1),
            None => End::Whole,
        },
        // `ascii_85::decode` stops at `~` and never reads the `>` after it.
        Some(b"ASCII85Decode" | b"A85") => match data.iter().position(|&b| b == b'~') {
            Some(i) => End::At(i + 1),
            None => End::Whole,
        },
        Some(b"RunLengthDecode" | b"RL") => run_length_end(data),
        Some(b"DCTDecode" | b"DCT") => match jpeg_end(data) {
            Some(e) => End::At(e),
            None => End::Unchecked("the JPEG data has no EOI the marker walk could reach"),
        },
        Some(b"LZWDecode" | b"LZW") => End::Unchecked("LZW end-of-data is not located"),
        Some(b"CCITTFaxDecode" | b"CCF") => End::Unchecked("CCITT end of data is not located"),
        Some(b"JBIG2Decode") => End::Unchecked("JBIG2 end of data is not located"),
        Some(b"JPXDecode") => End::Unchecked("JPEG 2000 codestream end is not located"),
        Some(_) => End::Unchecked("filter not recognised"),
        None => match image {
            Some(g) => match g.bytes() {
                Some(n) if (n as usize) < data.len() => End::At(n as usize),
                Some(_) => End::Whole,
                None => End::Unchecked("image size overflows"),
            },
            None => End::Whole,
        },
    }
}

/// Unfiltered image geometry: hayro reads `ceil(width × components × bpc / 8)
/// × height` bytes.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ImageGeometry {
    pub width: u64,
    pub height: u64,
    pub components: u64,
    pub bpc: u64,
}

impl ImageGeometry {
    fn bytes(self) -> Option<u64> {
        let row = self
            .width
            .checked_mul(self.components)?
            .checked_mul(self.bpc)?
            .div_ceil(8);
        row.checked_mul(self.height)
    }
}

/// hayro inflates with flate2's zlib reader, then a raw deflate reader, then
/// its own `FlateStream` (which skips a two-byte zlib header). Each stops at
/// the end of the deflate stream (the zlib reader after the Adler-32).
fn flate_end(data: &[u8]) -> End {
    match inflate_end(data, true) {
        Inflate::End(e) => return End::At(e),
        Inflate::TooLarge => return End::Unchecked("inflates past hayro's 512 MiB cap"),
        Inflate::Unterminated => return End::Whole,
        Inflate::Error => {}
    }
    match inflate_end(data, false) {
        Inflate::End(e) => return End::At(e),
        Inflate::TooLarge => return End::Unchecked("inflates past hayro's 512 MiB cap"),
        Inflate::Unterminated => return End::Whole,
        Inflate::Error => {}
    }
    match data.get(2..).map(|d| inflate_end(d, false)) {
        Some(Inflate::End(e)) => End::At(e + 2),
        _ => End::Unchecked("not a valid deflate stream; hayro's fallback decoder decides"),
    }
}

enum Inflate {
    End(usize),
    Unterminated,
    TooLarge,
    Error,
}

/// Inflate without keeping the output, to find where the stream ends.
fn inflate_end(data: &[u8], zlib: bool) -> Inflate {
    let mut z = flate2::Decompress::new(zlib);
    let mut buf = vec![0u8; 64 << 10];
    loop {
        let (in_before, out_before) = (z.total_in(), z.total_out());
        let Some(input) = usize::try_from(in_before).ok().and_then(|i| data.get(i..)) else {
            return Inflate::Error;
        };
        match z.decompress(input, &mut buf, flate2::FlushDecompress::None) {
            Err(_) => return Inflate::Error,
            Ok(flate2::Status::StreamEnd) => {
                return usize::try_from(z.total_in()).map_or(Inflate::Error, Inflate::End);
            }
            Ok(_) => {
                if z.total_out() > MAX_DECODED {
                    return Inflate::TooLarge;
                }
                if z.total_in() == in_before && z.total_out() == out_before {
                    return Inflate::Unterminated;
                }
            }
        }
    }
}

/// `run_length::decode`: a length byte 128 ends the data.
fn run_length_end(data: &[u8]) -> End {
    let mut i = 0usize;
    while let Some(&len) = data.get(i) {
        i += 1;
        match len {
            128 => return End::At(i),
            0..=127 => i += usize::from(len) + 1,
            _ => i += 1,
        }
    }
    End::Whole
}

/// Walk JPEG markers from SOI to EOI; returns the offset after EOI.
pub(crate) fn jpeg_end(d: &[u8]) -> Option<usize> {
    if !d.starts_with(&[0xFF, 0xD8]) {
        return None;
    }
    let mut i = 2usize;
    let seg_len = |at: usize| -> Option<usize> {
        let hi = *d.get(at)?;
        let lo = *d.get(at + 1)?;
        let n = usize::from(u16::from_be_bytes([hi, lo]));
        (n >= 2).then_some(n)
    };
    loop {
        // Decoders resynchronise on the next 0xFF; fill bytes repeat it.
        i += d.get(i..)?.iter().position(|&b| b == 0xFF)?;
        while d.get(i) == Some(&0xFF) {
            i += 1;
        }
        let m = *d.get(i)?;
        i += 1;
        match m {
            0xD9 => return Some(i),
            0x00 | 0x01 | 0xD0..=0xD7 => {}
            0xDA => {
                i = i.checked_add(seg_len(i)?)?;
                // Entropy-coded data runs to the next marker that is not a
                // stuffed 0xFF00 or a restart marker.
                loop {
                    i += d.get(i..)?.iter().position(|&b| b == 0xFF)?;
                    match *d.get(i + 1)? {
                        0x00 | 0xD0..=0xD7 | 0xFF => i += 1,
                        _ => break,
                    }
                }
            }
            _ => i = i.checked_add(seg_len(i)?)?,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn zlib(data: &[u8]) -> Vec<u8> {
        let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    #[test]
    fn flate_end_is_after_the_adler32() {
        let z = zlib(b"0 0 1 rg 0 0 10 10 re f");
        let mut with_tail = z.clone();
        with_tail.extend_from_slice(b"HIDDEN");
        assert_eq!(
            filter_end(Some(b"FlateDecode"), &with_tail, None),
            End::At(z.len())
        );
        assert_eq!(filter_end(Some(b"Fl"), &z, None), End::At(z.len()));
        // Truncated: hayro reads all of it.
        assert_eq!(
            filter_end(Some(b"FlateDecode"), &z[..z.len() / 2], None),
            End::Whole
        );
    }

    #[test]
    fn text_filters_and_run_length() {
        assert_eq!(filter_end(Some(b"AHx"), b"414243>tail", None), End::At(7));
        assert_eq!(
            filter_end(Some(b"A85"), b"87cURD]i,\"Ebo80~>tail", None),
            End::At(16)
        );
        assert_eq!(
            filter_end(Some(b"RL"), &[2, 1, 2, 3, 254, 9, 128, 7, 7], None),
            End::At(7)
        );
    }

    #[test]
    fn jpeg_end_skips_stuffed_bytes_and_restarts() {
        let mut j = vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 4, 1, 2, 0xFF, 0xDA, 0, 2];
        j.extend_from_slice(&[1, 0xFF, 0x00, 2, 0xFF, 0xD0, 3, 0xFF, 0xD9]);
        let n = j.len();
        j.extend_from_slice(b"tail");
        assert_eq!(jpeg_end(&j), Some(n));
    }

    #[test]
    fn unfiltered_images_read_their_declared_size() {
        let g = ImageGeometry {
            width: 3,
            height: 2,
            components: 1,
            bpc: 1,
        };
        assert_eq!(filter_end(None, &[0; 5], Some(g)), End::At(2));
        assert_eq!(filter_end(None, &[0; 2], Some(g)), End::Whole);
    }
}
