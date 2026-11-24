use core::ops::Range;

/// A parsed box header.
#[cfg_attr(not(feature = "inventory"), allow(dead_code))]
pub(crate) struct BoxHdr {
    pub(crate) start: u64,
    /// Header length: 8, or 16 with an `XLBox`.
    hdr: u64,
    /// End of the box, clamped to the enclosing range.
    pub(crate) end: u64,
    pub(crate) ty: [u8; 4],
    /// The declared length ran past the enclosing range.
    pub(crate) truncated: bool,
    /// `LBox == 0`: the box runs to the end of the enclosing range.
    pub(crate) to_end: bool,
}

impl BoxHdr {
    #[cfg(feature = "inventory")]
    pub(crate) fn range(&self) -> Range<u64> {
        self.start..self.end
    }
    pub(crate) fn payload(&self) -> Range<u64> {
        (self.start + self.hdr).min(self.end)..self.end
    }
}

/// Read a box header at `pos` inside `..limit`. `Err` carries why the bytes
/// cannot be a box (hayro-jpeg2000's `box::read` returns `None` for the same
/// cases and the parse loop stops).
pub(crate) fn read_box(data: &[u8], pos: u64, limit: u64) -> Result<BoxHdr, &'static str> {
    let rem = limit.saturating_sub(pos);
    if rem < 8 {
        return Err("fewer than 8 bytes left for a box header");
    }
    let at = pos as usize;
    let lbox = u32::from_be_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]);
    let ty = [data[at + 4], data[at + 5], data[at + 6], data[at + 7]];
    let (hdr, declared_end, to_end) = match lbox {
        0 => (8u64, limit, true),
        1 => {
            if rem < 16 {
                return Err("truncated XLBox");
            }
            let mut xl = [0u8; 8];
            xl.copy_from_slice(&data[at + 8..at + 16]);
            let xl = u64::from_be_bytes(xl);
            if xl < 16 {
                return Err("XLBox smaller than its own header");
            }
            (16, pos.saturating_add(xl), false)
        }
        2..=7 => return Err("box length smaller than its header"),
        n => (8, pos + u64::from(n), false),
    };
    let truncated = declared_end > limit;
    Ok(BoxHdr {
        start: pos,
        hdr,
        end: declared_end.min(limit),
        ty,
        truncated,
        to_end,
    })
}
