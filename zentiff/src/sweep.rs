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

/// Coarse compute-cost tier of a variant (`0` = cheapest). Made public so
/// the fleet harness and pickers can bound their candidate set the same way
/// [`plan_constrained`] does, and so a trained **scalar head** can read
/// compute alongside bytes. It is an **ordinal proxy**, not a calibrated
/// millisecond estimate — compare tiers, don't read absolute cost into them.
///
/// TIFF has **no continuous effort/level dial**: [`Compression`] is a method
/// enum, and the only level a method carries (DEFLATE's) is pinned upstream
/// to `Balanced`. So the compute axis *is* the compression **method ladder**,
/// and the tier is the method's place on it in ascending CPU cost:
///
/// | method         | tier | why                                            |
/// |----------------|------|------------------------------------------------|
/// | `Uncompressed` | 0    | memcpy — no coding at all                       |
/// | `PackBits`     | 1    | byte-level RLE — one cheap run scan             |
/// | `Lzw`          | 2    | dictionary coding — table build + lookups       |
/// | `Deflate`      | 3    | LZ77 match search **+ Huffman entropy coding**  |
///
/// `Deflate` tops the ladder because it is the only method that does real
/// entropy coding (LZ77 + Huffman), the most CPU per encode. `PackBits` is
/// the cheapest non-trivial method (a single byte-run pass), `Lzw` sits
/// between (dictionary build/lookup, no entropy stage).
///
/// The [`Predictor`] adds **+0**: horizontal differencing is a single linear
/// pass over the samples, negligible beside the compressor's work and well
/// below a method step — so it must not change the method tier. (If TIFF
/// ever exposes the DEFLATE level as a knob, fold it into the `Deflate` tier
/// as a sub-step **above** the base-3, below tier 4.)
#[must_use]
pub fn compute_tier(variant: &SweepVariant) -> u8 {
    // Method cost ordinal (ascending CPU). Exhaustive match: a new
    // `Compression` variant breaks this compile, forcing an explicit tier.
    let method_tier: u8 = match variant.compression {
        Compression::Uncompressed => 0,
        Compression::PackBits => 1,
        Compression::Lzw => 2,
        Compression::Deflate => 3,
    };
    // The predictor adds +0 — a single linear pass, negligible beside the
    // compressor and below a method step (see the doc comment). Kept as a
    // named term so a future per-method predictor cost is a one-line edit.
    let predictor_cost: u8 = match variant.predictor {
        Predictor::None | Predictor::Horizontal => 0,
    };
    method_tier + predictor_cost
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

    /// The densest coverage of TIFF's **compute axis** — for the trained
    /// **scalar head** (a continuous regression on the picker's compute
    /// dial). On a codec with a continuous effort knob this would ladder
    /// that knob; TIFF has **no scalar knob** (DEFLATE's level is pinned
    /// upstream), so its compute axis *is* the compression-**method**
    /// ladder, and "scalar dense" means **every compiled method** at the
    /// default predictor/layout. That gives the head the full
    /// compute-vs-bytes curve across methods — `Uncompressed` → `PackBits`
    /// → `Lzw` → `Deflate` ([`compute_tier`] 0..3) — instead of just the
    /// fast and strong ends.
    ///
    /// Equivalent to [`rd_core`](Self::rd_core) (both are "all compiled
    /// methods, default everything else"): with no scalar knob and no extra
    /// continuous dimension to densify, the RD-front *is* the dense compute
    /// sweep. It is spelled out as its own constructor so the cross-codec
    /// `scalar_dense` entry point exists uniformly, and so it stays correct
    /// if a future scalar knob (a DEFLATE-level axis) is added here without
    /// touching `rd_core`.
    ///
    /// Default-first via [`compiled_compressions`] (index 0 = the
    /// `Uncompressed` default); predictor and layout pinned to their
    /// defaults so only the method (compute) varies.
    #[must_use]
    pub fn scalar_dense() -> Self {
        Self {
            compressions: compiled_compressions(),
            predictors: vec![Predictor::None],
            big_tiff: vec![false],
        }
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
    /// Cell ids dropped because their [`compute_tier`] exceeded the
    /// `compute_limit` passed to [`plan_constrained`] — the explicit
    /// no-silent-caps report for the compute constraint (empty in the
    /// unconstrained [`plan`] path).
    pub compute_tier_skipped: Vec<String>,
}

/// Build the plan. Equivalent to [`plan_constrained`]`(axes, None, None)` —
/// the full, unconstrained curated space.
#[must_use]
pub fn plan(axes: &SweepAxes) -> SweepPlan {
    plan_constrained(axes, None, None)
}

/// Build the plan, optionally bounded by a compute budget and/or a deviation
/// scope.
///
/// - `compute_limit`: if `Some(max)`, cells whose [`compute_tier`] is `> max`
///   are dropped and their ids recorded in
///   [`SweepPlan::compute_tier_skipped`] (never silently capped) — the
///   compute-resource constraint a CPU-bound fleet or a "cheap methods only"
///   picker asks for.
/// - `max_deviations`: if `Some(n)`, only cells within `n` axis deviations of
///   the default survive (`0` = the default stratum only, `1` = main-effects
///   only). Present for **cross-codec API uniformity** — the fleet and picker
///   call this same shape on every codec — even though TIFF's curated space is
///   shallow (at most 2 deviations: predictor + BigTIFF).
///
/// `compute_limit` is applied first, then `max_deviations`.
#[must_use]
pub fn plan_constrained(
    axes: &SweepAxes,
    compute_limit: Option<u8>,
    max_deviations: Option<u8>,
) -> SweepPlan {
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
    let mut cells: Vec<SweepCell> = entries
        .into_iter()
        .map(|e| SweepCell {
            id: e.v.id(),
            fingerprint: fingerprint(&e.v),
            variant: e.v,
            deviations: e.deviations,
        })
        .collect();

    let mut compute_tier_skipped = Vec::new();
    if let Some(max) = compute_limit {
        cells.retain(|c| {
            if compute_tier(&c.variant) <= max {
                true
            } else {
                compute_tier_skipped.push(c.id.clone());
                false
            }
        });
    }
    if let Some(n) = max_deviations {
        cells.retain(|c| c.deviations <= n);
    }

    SweepPlan {
        cells,
        compute_tier_skipped,
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

    fn tier_of(c: Compression) -> u8 {
        compute_tier(&SweepVariant {
            compression: c,
            predictor: Predictor::None,
            big_tiff: false,
        })
    }

    #[test]
    fn compute_tier_orders_method_cost() {
        // The compute axis is the method ladder: the cheapest method
        // (Uncompressed = memcpy) must tier strictly below the most
        // expensive (Deflate = LZ77 + Huffman entropy coding).
        assert_eq!(
            tier_of(Compression::Uncompressed),
            0,
            "uncompressed is free"
        );
        assert!(
            tier_of(Compression::Uncompressed) < tier_of(Compression::Deflate),
            "uncompressed must cost less than DEFLATE entropy coding"
        );
        // Full monotonic ladder: none < packbits < lzw < deflate.
        assert!(tier_of(Compression::Uncompressed) < tier_of(Compression::PackBits));
        assert!(tier_of(Compression::PackBits) < tier_of(Compression::Lzw));
        assert!(tier_of(Compression::Lzw) < tier_of(Compression::Deflate));
        // The predictor folds in as +0 — it must not change the tier.
        let no_pred = SweepVariant {
            compression: Compression::Lzw,
            predictor: Predictor::None,
            big_tiff: false,
        };
        let with_pred = SweepVariant {
            compression: Compression::Lzw,
            predictor: Predictor::Horizontal,
            big_tiff: false,
        };
        assert_eq!(
            compute_tier(&no_pred),
            compute_tier(&with_pred),
            "the predictor adds +0 to the compute tier"
        );
    }

    #[test]
    fn scalar_dense_spans_the_method_ladder() {
        // TIFF has no scalar knob, so scalar_dense covers the *method*
        // (compute-tier) axis densely: every compiled method, default-first.
        let p = plan(&SweepAxes::scalar_dense());
        assert_eq!(
            p.cells[0].variant.compression,
            Compression::Uncompressed,
            "default (uncompressed) still first"
        );

        let compiled = compiled_compressions();
        let mut tiers: Vec<u8> = p.cells.iter().map(|c| compute_tier(&c.variant)).collect();
        tiers.sort_unstable();
        tiers.dedup();
        // One distinct tier per compiled method (each method has a unique
        // ordinal), and at least 3 under default features (none/lzw/deflate/
        // packbits all compiled). Adapt to whatever subset is compiled.
        assert_eq!(
            tiers.len(),
            compiled.len(),
            "one distinct compute tier per compiled method"
        );
        assert!(
            tiers.len() >= 3,
            "scalar_dense too sparse for a scalar head: {} tiers",
            tiers.len()
        );
        // The cheapest and most-expensive compiled methods are both covered.
        let cheapest = compiled.iter().map(|&c| tier_of(c)).min().unwrap();
        let priciest = compiled.iter().map(|&c| tier_of(c)).max().unwrap();
        assert!(tiers.contains(&cheapest), "cheapest method covered");
        assert!(tiers.contains(&priciest), "most-expensive method covered");
        // Default features compile DEFLATE, the top of the ladder.
        assert_eq!(priciest, tier_of(Compression::Deflate));
    }

    #[test]
    fn plan_constrained_drops_expensive_and_matches_plan() {
        let unconstrained = plan(&SweepAxes::scalar_dense());
        // A limit below the top method (DEFLATE = 3) but above the cheap
        // end: keeps none/packbits/lzw, drops deflate.
        let limit = 2u8;
        let limited = plan_constrained(&SweepAxes::scalar_dense(), Some(limit), None);
        assert!(!limited.cells.is_empty());
        assert!(
            limited.cells.len() < unconstrained.cells.len(),
            "the compute limit must drop the expensive cells"
        );
        assert!(
            limited
                .cells
                .iter()
                .all(|c| compute_tier(&c.variant) <= limit),
            "every surviving cell must be within budget"
        );
        assert!(
            !limited.compute_tier_skipped.is_empty(),
            "dropped cells must be reported, never silently capped"
        );

        // The unconstrained delegate must equal plan() cell-for-cell.
        let via_constrained = plan_constrained(&SweepAxes::scalar_dense(), None, None);
        let direct = plan(&SweepAxes::scalar_dense());
        assert_eq!(via_constrained.cells.len(), direct.cells.len());
        for (x, y) in via_constrained.cells.iter().zip(&direct.cells) {
            assert_eq!(x.id, y.id);
            assert_eq!(x.fingerprint, y.fingerprint);
        }
        assert!(via_constrained.compute_tier_skipped.is_empty());

        // max_deviations narrows to the default stratum on the full space.
        let full = plan(&SweepAxes::modes_full());
        let mains_only = plan_constrained(&SweepAxes::modes_full(), None, Some(0));
        assert!(
            mains_only.cells.iter().all(|c| c.deviations == 0),
            "max_deviations=0 keeps only the default stratum"
        );
        assert!(mains_only.cells.len() < full.cells.len());
    }
}
