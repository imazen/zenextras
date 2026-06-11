//! Sweep-plan construction over the TIFF encoder knob space.
//!
//! Port of the variant-generation playbook
//! (`zenjpeg/docs/VARIANT_GENERATION.md`). TIFF is lossless, so the
//! **entire curated space is trial-class**: decoded pixels are
//! identical across every cell, `min(bytes)` is exact, no quality grid
//! exists, and the whole `modes_full` is ≤ 16 cells.
//!
//! Build-feature liveness (playbook pattern 10): `Lzw` and `Deflate`
//! are cargo-feature-gated; the axes contain only compiled methods and
//! [`variant_from_cell_id`] rejects ids naming methods this build
//! cannot encode (encoding would error at runtime — better to reject
//! the identity at declare time).
//!
//! Notes on what is NOT an axis:
//!
//! - Deflate's level is pinned upstream (`DeflateLevel::Balanced` in
//!   `Compression::to_tiff`) — a level axis joins when zentiff exposes
//!   the knob.
//! - `Predictor::Horizontal` is **gate-shadowed under `Uncompressed`**
//!   (byte-identical output — the predictor only transforms data that a
//!   compressor consumes; falsified-by-harness 2026-06-11), so the
//!   predictor axis structurally crosses compressing methods only
//!   (pattern 10).
//! - PackBits is BYTE-level RLE: on RGB content whose runs are
//!   pixel-level (3-byte periods), it has no byte runs to find and emits
//!   *larger* output than uncompressed — expected behavior, not a bug;
//!   its wins live on gray/palette-style byte-run content.
//! - `big_tiff` is container layout (trial-class byte overhead) — an
//!   axis value, useful for the tiny-file end of the size sweep.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::encode::{Compression, Predictor, TiffEncodeConfig};

/// One encode variant (all trial-class).
#[derive(Clone, Debug)]
pub struct SweepVariant {
    /// Compression method (compiled set only).
    pub compression: Compression,
    /// Sample predictor.
    pub predictor: Predictor,
    /// BigTIFF container layout.
    pub big_tiff: bool,
}

impl SweepVariant {
    /// Whether the predictor knob applies to this method (it transforms
    /// data a compressor consumes; under `Uncompressed` it is
    /// byte-inert — proven by the validation harness).
    #[must_use]
    pub fn predictor_applies(compression: Compression) -> bool {
        compression != Compression::Uncompressed
    }

    /// Build the encode config for this variant.
    #[must_use]
    pub fn build(&self) -> TiffEncodeConfig {
        TiffEncodeConfig {
            compression: self.compression,
            predictor: self.predictor,
            big_tiff: self.big_tiff,
            ..TiffEncodeConfig::default()
        }
    }

    fn id(&self) -> String {
        let mut s = format!("tiff-{}", compression_token(self.compression));
        if Self::predictor_applies(self.compression) && self.predictor == Predictor::Horizontal {
            s.push_str("-hpred");
        }
        if self.big_tiff {
            s.push_str("-big");
        }
        s
    }
}

fn compression_token(c: Compression) -> &'static str {
    match c {
        Compression::Uncompressed => "none",
        Compression::Lzw => "lzw",
        Compression::Deflate => "deflate",
        // In-crate match is exhaustive: a new `Compression` variant
        // breaks this compile, forcing the token AND parser update
        // together (the roundtrip test enforces the pairing).
        Compression::PackBits => "packbits",
    }
}

/// The compression methods THIS build can encode, default first.
#[must_use]
pub fn compiled_compressions() -> Vec<Compression> {
    let mut v = vec![Compression::Uncompressed];
    #[cfg(feature = "lzw")]
    v.push(Compression::Lzw);
    #[cfg(feature = "deflate")]
    v.push(Compression::Deflate);
    v.push(Compression::PackBits);
    v
}

/// Reconstruct the [`SweepVariant`] a cell id denotes. Grammar:
/// `tiff-<method>[-hpred][-big]`. Ids naming methods this build cannot
/// encode error (pattern 10). Renderer and parser move in lockstep
/// (`cell_ids_roundtrip_to_their_variants`); evolution is
/// additive-only.
pub fn variant_from_cell_id(id: &str) -> Result<SweepVariant, String> {
    let Some(rest) = id.strip_prefix("tiff-") else {
        return Err(format!("cell id {id:?} is not a tiff- id"));
    };
    let mut parts = rest.split('-');
    let tok = parts.next().unwrap_or_default();
    let compression = compiled_compressions()
        .into_iter()
        .find(|c| compression_token(*c) == tok)
        .ok_or_else(|| {
            format!(
                "compression {tok:?} in {id:?} is not encodable in this build \
                 (cargo features gate lzw/deflate)"
            )
        })?;
    let mut v = SweepVariant {
        compression,
        predictor: Predictor::None,
        big_tiff: false,
    };
    for f in parts {
        match f {
            "hpred" if SweepVariant::predictor_applies(compression) => {
                v.predictor = Predictor::Horizontal;
            }
            "hpred" => {
                return Err(format!(
                    "predictor flag in {id:?} but {tok:?} is uncompressed (predictor is inert there)"
                ));
            }
            "big" => v.big_tiff = true,
            other => return Err(format!("unknown flag {other:?} in {id:?}")),
        }
    }
    Ok(v)
}

/// Byte-identity fingerprint over resolved state (every field hashed —
/// all trial-class, nothing to exclude).
#[must_use]
pub fn fingerprint(variant: &SweepVariant) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let mut write = |b: u8| {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    };
    write(match variant.compression {
        Compression::Uncompressed => 0,
        Compression::Lzw => 1,
        Compression::Deflate => 2,
        Compression::PackBits => 3,
    });
    write(u8::from(
        SweepVariant::predictor_applies(variant.compression)
            && variant.predictor == Predictor::Horizontal,
    ));
    write(u8::from(variant.big_tiff));
    h
}

/// Axes, most-important value first.
#[derive(Clone, Debug)]
pub struct SweepAxes {
    /// Compression methods (compiled set only; index 0 = default).
    pub compressions: Vec<Compression>,
    /// Predictors.
    pub predictors: Vec<Predictor>,
    /// BigTIFF layout.
    pub big_tiff: Vec<bool>,
}

impl SweepAxes {
    /// RD-front: the compiled methods at default predictor/layout.
    #[must_use]
    pub fn rd_core() -> Self {
        Self {
            compressions: compiled_compressions(),
            predictors: vec![Predictor::None],
            big_tiff: vec![false],
        }
    }

    /// The full curated space (≤ 16 cells).
    #[must_use]
    pub fn modes_full() -> Self {
        let mut axes = Self::rd_core();
        axes.predictors.push(Predictor::Horizontal);
        axes.big_tiff.push(true);
        axes
    }
}

/// One encode cell.
#[derive(Clone, Debug)]
pub struct SweepCell {
    /// Stable id (`tiff-<method>[-hpred][-big]`).
    pub id: String,
    /// The variant to encode with.
    pub variant: SweepVariant,
    /// Byte-identity fingerprint of the resolved state.
    pub fingerprint: u64,
    /// Axes deviating from the default stratum.
    pub deviations: u8,
}

/// The finite plan (no budget ladder needed at ≤ 16 cells; no quality
/// grid — TIFF is lossless).
#[derive(Clone, Debug)]
pub struct SweepPlan {
    /// Cells, main-effects-first.
    pub cells: Vec<SweepCell>,
}

/// Build the plan.
#[must_use]
pub fn plan(axes: &SweepAxes) -> SweepPlan {
    struct Entry {
        v: SweepVariant,
        deviations: u8,
        idx_sum: usize,
    }
    let mut entries = Vec::new();
    for (ci, &compression) in axes.compressions.iter().enumerate() {
        let predictor_options: Vec<Predictor> = if SweepVariant::predictor_applies(compression) {
            axes.predictors.clone()
        } else {
            vec![axes.predictors[0]]
        };
        for (pi, &predictor) in predictor_options.iter().enumerate() {
            for (bi, &big_tiff) in axes.big_tiff.iter().enumerate() {
                entries.push(Entry {
                    v: SweepVariant {
                        compression,
                        predictor,
                        big_tiff,
                    },
                    deviations: u8::from(ci != 0) + u8::from(pi != 0) + u8::from(bi != 0),
                    idx_sum: ci + pi + bi,
                });
            }
        }
    }
    entries.sort_by_key(|e| (e.deviations, e.idx_sum));
    SweepPlan {
        cells: entries
            .into_iter()
            .map(|e| SweepCell {
                id: e.v.id(),
                fingerprint: fingerprint(&e.v),
                variant: e.v,
                deviations: e.deviations,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_ids_roundtrip_to_their_variants() {
        let p = plan(&SweepAxes::modes_full());
        assert!(p.cells.len() >= 8);
        assert_eq!(p.cells[0].deviations, 0);
        for cell in &p.cells {
            let v = variant_from_cell_id(&cell.id).unwrap_or_else(|e| panic!("{}: {e}", cell.id));
            assert_eq!(fingerprint(&v), cell.fingerprint, "drift for {}", cell.id);
        }
        // Unique ids, non-decreasing deviations.
        for w in p.cells.windows(2) {
            assert!(w[1].deviations >= w[0].deviations);
            assert_ne!(w[0].id, w[1].id);
        }
    }

    #[test]
    fn malformed_ids_error() {
        for bad in ["tiff-zstd", "tiff-lzw-warp", "png-lzw", "tiff-lzw-hpred-x"] {
            assert!(variant_from_cell_id(bad).is_err(), "{bad:?}");
        }
    }
}
