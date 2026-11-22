//! Regression tests for zensvg DoS / adversarial guards:
//! - zenextras#40: roxmltree internal XML entity amplification across all entry points
//!   (`decode`, `render`, `probe`, `probe_full`, `output_info`, `svg_dimensions`,
//!   `parse_svg`, `inventory`, `optimize`) and tight `max_memory` bounds.
//! - zenextras#41: 3+-node and 1+/2-node resource/paint/container cycles across
//!   `url(#id)` attributes, container nesting, `<use>`, `<feImage>`, CSS `<style>`,
//!   inline `style`, and attribute inheritance.
//! - zenextras#42: `href` / `xlink:href` cycles and lasso loops in `usvg::HrefIter`.
//! - Positive tests verifying shared `<defs>` DAG reuse, non-cyclic inheritance,
//!   hyperlinks, and benign entities continue to succeed and render accurately.

use std::borrow::Cow;
use std::io::Write as _;
use std::time::{Duration, Instant};

use zencodec::decode::{Decode, DecodeJob, DecoderConfig};
use zencodec::{CodecErrorExt, LimitExceeded, ResourceLimits};
#[cfg(feature = "optimize")]
use zensvg::SvgError;
use zensvg::{RenderOptions, SvgDecoderConfig, render, svg_dimensions};

fn within<T: Send + 'static>(secs: u64, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(Duration::from_secs(secs))
        .unwrap_or_else(|_| panic!("operation did not finish within {secs} s"))
}

fn wrap_svg(body: &str) -> Vec<u8> {
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="20" height="20">{body}</svg>"#
    )
    .into_bytes()
}

fn gzip(data: &[u8]) -> Vec<u8> {
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(data).unwrap();
    enc.finish().unwrap()
}

fn assert_rejected_all_entrypoints(name: &str, data: &[u8]) {
    let name = name.to_string();
    let d = data.to_vec();
    within(5, move || {
        let opts = RenderOptions {
            load_system_fonts: false,
            ..Default::default()
        };
        assert!(
            render(&d, &opts).is_err(),
            "{name}: render unexpectedly succeeded"
        );
        assert!(
            zensvg::render::parse_svg(&d, &opts).is_err(),
            "{name}: parse_svg unexpectedly succeeded"
        );
        assert!(
            svg_dimensions(&d, &opts).is_err(),
            "{name}: svg_dimensions unexpectedly succeeded"
        );
        let cfg = SvgDecoderConfig::new();
        assert!(
            cfg.clone().job().probe(&d).is_err(),
            "{name}: probe unexpectedly succeeded"
        );
        assert!(
            cfg.clone().job().probe_full(&d).is_err(),
            "{name}: probe_full unexpectedly succeeded"
        );
        assert!(
            cfg.clone().job().output_info(&d).is_err(),
            "{name}: output_info unexpectedly succeeded"
        );
        let dec_res = cfg
            .clone()
            .job()
            .decoder(Cow::Borrowed(&d), &[])
            .and_then(|dec| dec.decode());
        assert!(dec_res.is_err(), "{name}: decode unexpectedly succeeded");

        #[cfg(feature = "inventory")]
        {
            use zencodec::inventory::{Disposition, PartTag};

            let inv = cfg.job().inventory(&d).expect("inventory completes");
            if d.starts_with(b"\x1f\x8b") {
                assert!(
                    inv.parts()
                        .iter()
                        .all(|p| p.disposition() != Disposition::ImageData),
                    "{name}: svgz inventory reported ImageData for rejected input\n{inv}"
                );
                assert!(
                    inv.parts().iter().any(|p| {
                        *p.tag() == PartTag::Name("deflate".into())
                            && p.disposition() == Disposition::Dropped
                    }),
                    "{name}: svgz deflate payload should be Dropped\n{inv}"
                );
            } else {
                assert!(
                    inv.parts().iter().all(|p| !p.disposition().is_consumed()),
                    "{name}: inventory consumed bytes of rejected input\n{inv}"
                );
            }
        }
    });
}

// ── Issue #40: entity amplification ─────────────────────────────────────────

#[test]
fn issue40_exact_reproducer_rejected_across_all_entrypoints() {
    // Exact shape from imazen/zenextras#40:
    // 60,000-byte internal entity referenced 4,000 times (~72 KB -> ~240 MB).
    let val = "x".repeat(60_000);
    let refs = "&a;".repeat(4_000);
    let svg = format!(
        r#"<!DOCTYPE svg [<!ENTITY a "{val}">]><svg xmlns="http://www.w3.org/2000/svg" width="20" height="20"><desc>{refs}</desc><rect width="20" height="20" fill="red"/></svg>"#
    )
    .into_bytes();
    assert_eq!(svg.len(), 72_154);

    let t0 = Instant::now();
    assert_rejected_all_entrypoints("issue40_plain", &svg);
    assert_rejected_all_entrypoints("issue40_svgz", &gzip(&svg));

    #[cfg(feature = "optimize")]
    {
        let err = zensvg::optimize(&svg, &zensvg::OptimizeOptions::default()).unwrap_err();
        assert!(
            matches!(err, SvgError::Parse(_)),
            "optimize expected Parse error, got {err:?}"
        );
        let err_gz =
            zensvg::optimize(&gzip(&svg), &zensvg::OptimizeOptions::default()).unwrap_err();
        assert!(
            matches!(err_gz, SvgError::Parse(_)),
            "optimize svgz expected Parse error, got {err_gz:?}"
        );
    }

    assert!(
        t0.elapsed() < Duration::from_secs(2),
        "entity bomb checks took too long: {:?}",
        t0.elapsed()
    );
}

#[test]
fn issue40_entity_bypasses_are_closed() {
    // 1) Quoted "SYSTEM" or "PUBLIC" inside an internal entity literal must NOT
    //    bypass expansion accounting.
    let val_sys = format!("SYSTEM {}", "x".repeat(60_000));
    let refs = "&a;".repeat(4_000);
    let quoted_system = format!(
        r#"<!DOCTYPE svg [<!ENTITY a "{val_sys}">]><svg xmlns="http://www.w3.org/2000/svg" width="20" height="20"><desc>{refs}</desc><rect width="20" height="20"/></svg>"#
    )
    .into_bytes();
    assert_rejected_all_entrypoints("quoted_system_entity", &quoted_system);

    let val_pub = format!("PUBLIC {}", "x".repeat(60_000));
    let quoted_public = format!(
        r#"<!DOCTYPE svg [<!ENTITY a "{val_pub}">]><svg xmlns="http://www.w3.org/2000/svg" width="20" height="20"><desc>{refs}</desc><rect width="20" height="20"/></svg>"#
    )
    .into_bytes();
    assert_rejected_all_entrypoints("quoted_public_entity", &quoted_public);

    // 2) Entity bomb inside a malformed start tag (`XKind::Malformed`) must be
    //    rejected before `roxmltree` normalizes attributes.
    let val = "x".repeat(60_000);
    let malformed_tag = format!(
        r#"<!DOCTYPE svg [<!ENTITY a "{val}">]><svg xmlns="http://www.w3.org/2000/svg" width="20" height="20"><rect attr="{refs}" !bad/></svg>"#
    )
    .into_bytes();
    assert_rejected_all_entrypoints("malformed_tag_entity", &malformed_tag);

    #[cfg(feature = "optimize")]
    {
        assert!(zensvg::optimize(&quoted_system, &zensvg::OptimizeOptions::default()).is_err());
        assert!(zensvg::optimize(&malformed_tag, &zensvg::OptimizeOptions::default()).is_err());
    }
}

#[test]
fn issue40_many_unterminated_ampersands_are_linear_time() {
    // 100,000 bare `&` bytes with no `;` must not degrade to O(N^2).
    let amps = "&".repeat(100_000);
    let svg = format!(
        r#"<!DOCTYPE svg [<!ENTITY a "ok">]><svg xmlns="http://www.w3.org/2000/svg" width="4" height="4"><desc>{amps}</desc><rect width="4" height="4" fill="red"/></svg>"#
    )
    .into_bytes();
    let t0 = Instant::now();
    // roxmltree rejects bare `&` as MalformedEntityReference, quickly.
    let _ = render(&svg, &RenderOptions::default());
    assert!(
        t0.elapsed() < Duration::from_secs(1),
        "bare `&` scan took {:?}",
        t0.elapsed()
    );
}

#[test]
fn issue40_max_memory_tightens_entity_budget_without_false_positives() {
    // 20 KB entity referenced 20 times = 400 KB expansion:
    // - Allowed under default budget (max(input*8, 1 MiB) = 1 MiB)
    // - Rejected under max_memory = 1 MiB (max_memory / 4 = 256 KiB) with LimitExceeded::Memory.
    let val = "x".repeat(20_000);
    let refs = "&a;".repeat(20);
    let svg = format!(
        r#"<!DOCTYPE svg [<!ENTITY a "{val}">]><svg xmlns="http://www.w3.org/2000/svg" width="4" height="4"><desc>{refs}</desc><rect width="4" height="4" fill="red"/></svg>"#
    )
    .into_bytes();

    let ok_out = SvgDecoderConfig::new()
        .job()
        .decoder(Cow::Borrowed(&svg), &[])
        .unwrap()
        .decode()
        .expect("400 KB benign entity fits default 1 MiB budget");
    assert_eq!(ok_out.info().width, 4);

    let tight = ResourceLimits::none().with_max_memory(1 << 20);
    let err = SvgDecoderConfig::new()
        .job()
        .with_limits(tight)
        .decoder(Cow::Borrowed(&svg), &[])
        .unwrap()
        .decode()
        .unwrap_err();
    assert!(
        matches!(err.limit_exceeded(), Some(LimitExceeded::Memory { .. })),
        "expected Memory limit exceeded, got {err:?}"
    );
    assert!(
        SvgDecoderConfig::new()
            .job()
            .with_limits(tight)
            .probe(&svg)
            .is_err()
    );
    assert!(
        SvgDecoderConfig::new()
            .job()
            .with_limits(tight)
            .probe_full(&svg)
            .is_err()
    );
    assert!(
        SvgDecoderConfig::new()
            .job()
            .with_limits(tight)
            .output_info(&svg)
            .is_err()
    );
}

#[test]
fn issue40_benign_entities_still_render_and_optimize() {
    let svg = br##"<!DOCTYPE svg [
        <!ENTITY ns "http://www.w3.org/2000/svg">
        <!ENTITY red "#ff0000">
        <!ENTITY lt "&#38;#60;">
    ]>
    <svg xmlns="&ns;" width="10" height="10">
        <desc>&lt;&amp;&gt;&quot;&apos;</desc>
        <rect width="10" height="10" fill="&red;"/>
    </svg>"##;

    let out = render(svg, &RenderOptions::default()).unwrap();
    assert_eq!(&out.data[0..4], &[255, 0, 0, 255]);

    let dec = SvgDecoderConfig::new()
        .job()
        .with_limits(ResourceLimits::none().with_max_memory(64 << 20))
        .decoder(Cow::Borrowed(svg), &[])
        .unwrap()
        .decode()
        .unwrap();
    assert_eq!(dec.info().width, 10);

    #[cfg(feature = "optimize")]
    {
        let opt = zensvg::optimize(svg, &zensvg::OptimizeOptions::default()).unwrap();
        assert!(!opt.is_empty());
    }
}

// ── Issues #41 & #42: cycles >= 1 and href lassos ───────────────────────────

#[test]
fn issue41_exact_reproducer_and_resource_cycles_rejected() {
    // Exact 524-byte 3-pattern cycle from imazen/zenextras#41.
    let p3 = br##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="20" height="20"><pattern id="p1" width="4" height="4" patternUnits="userSpaceOnUse"><rect width="4" height="4" fill="url(#p2)"/></pattern><pattern id="p2" width="4" height="4" patternUnits="userSpaceOnUse"><rect width="4" height="4" fill="url(#p3)"/></pattern><pattern id="p3" width="4" height="4" patternUnits="userSpaceOnUse"><rect width="4" height="4" fill="url(#p1)"/></pattern><rect width="20" height="20" fill="url(#p1)"/></svg>"##;
    assert_eq!(p3.len(), 524);
    assert_rejected_all_entrypoints("issue41_exact_3cycle", p3);

    // Length-1 and length-2 pattern cycles.
    assert_rejected_all_entrypoints(
        "pattern_1cycle",
        &wrap_svg(
            r##"<pattern id="p" width="4" height="4" patternUnits="userSpaceOnUse"><rect width="4" height="4" fill="url(#p)"/></pattern><rect width="20" height="20" fill="url(#p)"/>"##,
        ),
    );
    assert_rejected_all_entrypoints(
        "pattern_2cycle",
        &wrap_svg(
            r##"<pattern id="p1" width="4" height="4" patternUnits="userSpaceOnUse"><rect width="4" height="4" fill="url(#p2)"/></pattern><pattern id="p2" width="4" height="4" patternUnits="userSpaceOnUse"><rect width="4" height="4" fill="url(#p1)"/></pattern><rect width="20" height="20" fill="url(#p1)"/>"##,
        ),
    );

    // Stroke, clipPath, mask, marker, and <use> cycles (lengths 1 and 3).
    assert_rejected_all_entrypoints(
        "stroke_3cycle",
        &wrap_svg(
            r##"<pattern id="p1" width="4" height="4"><rect width="4" height="4" fill="none" stroke="url(#p2)"/></pattern><pattern id="p2" width="4" height="4"><rect width="4" height="4" fill="none" stroke="url(#p3)"/></pattern><pattern id="p3" width="4" height="4"><rect width="4" height="4" fill="none" stroke="url(#p1)"/></pattern><rect width="20" height="20" stroke="url(#p1)"/>"##,
        ),
    );
    assert_rejected_all_entrypoints(
        "clippath_attr_1cycle",
        &wrap_svg(
            r##"<clipPath id="c" clip-path="url(#c)"><rect width="10" height="10"/></clipPath><rect width="20" height="20" clip-path="url(#c)"/>"##,
        ),
    );
    assert_rejected_all_entrypoints(
        "clippath_child_3cycle",
        &wrap_svg(
            r##"<clipPath id="c1"><rect width="10" height="10" clip-path="url(#c2)"/></clipPath><clipPath id="c2"><rect width="10" height="10" clip-path="url(#c3)"/></clipPath><clipPath id="c3"><rect width="10" height="10" clip-path="url(#c1)"/></clipPath><rect width="20" height="20" clip-path="url(#c1)"/>"##,
        ),
    );
    assert_rejected_all_entrypoints(
        "mask_child_3cycle",
        &wrap_svg(
            r##"<mask id="m1"><rect width="10" height="10" mask="url(#m2)"/></mask><mask id="m2"><rect width="10" height="10" mask="url(#m3)"/></mask><mask id="m3"><rect width="10" height="10" mask="url(#m1)"/></mask><rect width="20" height="20" mask="url(#m1)"/>"##,
        ),
    );
    assert_rejected_all_entrypoints(
        "mask_attr_1cycle",
        &wrap_svg(
            r##"<mask id="m1" mask="url(#m1)"><rect width="10" height="10" fill="white"/></mask><rect width="20" height="20" mask="url(#m1)"/>"##,
        ),
    );
    assert_rejected_all_entrypoints(
        "marker_3cycle",
        &wrap_svg(
            r##"<marker id="k1"><path d="M0 0 L4 4" marker-end="url(#k2)"/></marker><marker id="k2"><path d="M0 0 L4 4" marker-end="url(#k3)"/></marker><marker id="k3"><path d="M0 0 L4 4" marker-end="url(#k1)"/></marker><path d="M0 0 L20 20" marker-end="url(#k1)"/>"##,
        ),
    );
    assert_rejected_all_entrypoints(
        "use_3cycle",
        &wrap_svg(
            r##"<g id="g1"><use href="#g2"/></g><g id="g2"><use href="#g3"/></g><g id="g3"><use href="#g1"/></g><use href="#g1"/>"##,
        ),
    );
    assert_rejected_all_entrypoints(
        "use_1cycle",
        &wrap_svg(r##"<g id="g1"><use xlink:href="#g1"/></g><use href="#g1"/>"##),
    );
}

#[test]
fn issue41_cross_type_css_style_and_inherit_cycles_rejected() {
    // 2-node cross-type cycle: pattern -> filter -> feImage -> rect(fill=pattern)
    // (`usvg` does NOT break this and overflows the stack without our guard).
    assert_rejected_all_entrypoints(
        "cross_type_pattern_filter_feimage",
        &wrap_svg(
            r##"<defs><pattern id="p" width="4" height="4" patternUnits="userSpaceOnUse"><rect width="4" height="4" filter="url(#f)"/></pattern><filter id="f"><feImage href="#r"/></filter><rect id="r" width="4" height="4" fill="url(#p)"/></defs><rect width="20" height="20" fill="url(#p)"/>"##,
        ),
    );

    // 1-node filter -> feImage -> g -> rect(filter) cycle (`usvg` overflows).
    assert_rejected_all_entrypoints(
        "filter_feimage_descendant_cycle",
        &wrap_svg(
            r##"<defs><filter id="f"><feImage href="#g"/></filter><g id="g"><rect width="4" height="4" filter="url(#f)"/></g></defs><rect width="20" height="20" filter="url(#f)"/>"##,
        ),
    );

    // Mixed href inheritance + fill cycle: pattern(href=#p2) + p2(rect(fill=url(#p1))).
    assert_rejected_all_entrypoints(
        "pattern_href_plus_fill_cycle",
        &wrap_svg(
            r##"<defs><pattern id="p1" width="4" height="4" patternUnits="userSpaceOnUse" href="#p2"/><pattern id="p2" width="4" height="4" patternUnits="userSpaceOnUse"><rect width="4" height="4" fill="url(#p1)"/></pattern></defs><rect width="20" height="20" fill="url(#p1)"/>"##,
        ),
    );

    // Cycle hidden in `<style>` CSS rules.
    assert_rejected_all_entrypoints(
        "css_style_cycle",
        &wrap_svg(
            r##"<style>.a{fill:url(#p2)} .b{fill:url(#p3)} .c{fill:url(#p1)}</style><pattern id="p1" width="4" height="4"><rect class="a" width="4" height="4"/></pattern><pattern id="p2" width="4" height="4"><rect class="b" width="4" height="4"/></pattern><pattern id="p3" width="4" height="4"><rect class="c" width="4" height="4"/></pattern><rect width="20" height="20" fill="url(#p1)"/>"##,
        ),
    );

    // Cycle hidden in inline `style="..."` attribute.
    assert_rejected_all_entrypoints(
        "inline_style_cycle",
        &wrap_svg(
            r##"<pattern id="p1" width="4" height="4"><rect style="fill:url(#p2)" width="4" height="4"/></pattern><pattern id="p2" width="4" height="4"><rect style="fill:url(#p1)" width="4" height="4"/></pattern><rect width="20" height="20" fill="url(#p1)"/>"##,
        ),
    );

    // Cycle via inherited paint inside resource container: `<g fill="url(#p1)"><pattern id="p1"><rect/></pattern></g>`.
    assert_rejected_all_entrypoints(
        "inherited_paint_into_pattern_cycle",
        &wrap_svg(
            r##"<g fill="url(#p1)"><pattern id="p1" width="4" height="4" patternUnits="userSpaceOnUse"><rect width="4" height="4"/></pattern><rect width="20" height="20"/></g>"##,
        ),
    );
}

#[test]
fn issue42_exact_reproducer_and_href_cycles_rejected() {
    // Exact 447-byte lasso reproducer from imazen/zenextras#42.
    let exact = wrap_svg(
        r##"<pattern id="q1" width="4" height="4" patternUnits="userSpaceOnUse" href="#q2"/><pattern id="q2" width="4" height="4" patternUnits="userSpaceOnUse" href="#q3"/><pattern id="q3" width="4" height="4" patternUnits="userSpaceOnUse" href="#q2"><rect width="4" height="4" fill="#00f"/></pattern><rect width="20" height="20" fill="url(#q1)"/>"##,
    );
    assert_eq!(exact.len(), 447);
    assert_rejected_all_entrypoints("issue42_exact_pattern_lasso", &exact);

    // xlink:href lasso, gradient 2-stop / 1-stop / 0-stop lassos, filter lasso,
    // length-1 self-loop, and length-2 loop.
    assert_rejected_all_entrypoints(
        "issue42_xlink_pattern_lasso",
        &wrap_svg(
            r##"<pattern id="q1" width="4" height="4" xlink:href="#q2"/><pattern id="q2" width="4" height="4" xlink:href="#q3"/><pattern id="q3" width="4" height="4" xlink:href="#q2"><rect width="4" height="4" fill="#00f"/></pattern><rect width="20" height="20" fill="url(#q1)"/>"##,
        ),
    );
    assert_rejected_all_entrypoints(
        "issue42_gradient_2stop_lasso",
        &wrap_svg(
            r##"<linearGradient id="g1" href="#g2"/><linearGradient id="g2" href="#g3"/><linearGradient id="g3" href="#g2"><stop offset="0" stop-color="#00f"/><stop offset="1" stop-color="#f00"/></linearGradient><rect width="20" height="20" fill="url(#g1)"/>"##,
        ),
    );
    assert_rejected_all_entrypoints(
        "issue42_gradient_1stop_lasso",
        &wrap_svg(
            r##"<linearGradient id="g1" href="#g2"/><linearGradient id="g2" href="#g3"/><linearGradient id="g3" href="#g2"><stop offset="0" stop-color="#00f"/></linearGradient><rect width="20" height="20" fill="url(#g1)"/>"##,
        ),
    );
    assert_rejected_all_entrypoints(
        "issue42_filter_lasso",
        &wrap_svg(
            r##"<filter id="f1" href="#f2"/><filter id="f2" href="#f3"/><filter id="f3" href="#f2"><feFlood flood-color="#00f"/></filter><rect width="20" height="20" fill="#0f0" filter="url(#f1)"/>"##,
        ),
    );
    assert_rejected_all_entrypoints(
        "href_1cycle",
        &wrap_svg(
            r##"<pattern id="q1" width="4" height="4" href="#q1"/><rect width="20" height="20" fill="url(#q1)"/>"##,
        ),
    );
    assert_rejected_all_entrypoints(
        "href_2cycle",
        &wrap_svg(
            r##"<pattern id="q1" width="4" height="4" href="#q2"/><pattern id="q2" width="4" height="4" href="#q1"><rect width="4" height="4" fill="#00f"/></pattern><rect width="20" height="20" fill="url(#q1)"/>"##,
        ),
    );
}

#[test]
fn legitimate_defs_dag_reuse_and_non_cyclic_chains_succeed() {
    // Diamond DAG sharing gradients, patterns, clipPaths, masks, filters,
    // markers, and `<use>` templates across multiple painted shapes.
    let dag = br##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="40" height="40">
        <defs>
            <linearGradient id="g_base">
                <stop offset="0" stop-color="#ff0000"/>
                <stop offset="1" stop-color="#0000ff"/>
            </linearGradient>
            <linearGradient id="g1" xlink:href="#g_base" x1="0" y1="0" x2="1" y2="1"/>
            <linearGradient id="g2" href="#g_base" x1="1" y1="0" x2="0" y2="1"/>
            <pattern id="p_base" width="4" height="4" patternUnits="userSpaceOnUse">
                <rect width="4" height="4" fill="url(#g1)"/>
            </pattern>
            <pattern id="p1" href="#p_base" width="4" height="4" patternUnits="userSpaceOnUse"/>
            <pattern id="p2" width="4" height="4" patternUnits="userSpaceOnUse">
                <rect width="4" height="4" fill="url(#g2)"/>
            </pattern>
            <clipPath id="c_base">
                <rect width="40" height="40"/>
            </clipPath>
            <clipPath id="c1" clip-path="url(#c_base)">
                <circle cx="20" cy="20" r="20"/>
            </clipPath>
            <mask id="m1">
                <rect width="40" height="40" fill="white"/>
            </mask>
            <filter id="f_base">
                <feGaussianBlur stdDeviation="0"/>
            </filter>
            <filter id="f1" href="#f_base"/>
            <marker id="mk1" markerWidth="4" markerHeight="4" refX="2" refY="2">
                <circle cx="2" cy="2" r="2" fill="url(#g1)"/>
            </marker>
            <rect id="tpl" width="10" height="10" fill="url(#g1)"/>
        </defs>
        <g id="top" clip-path="url(#c1)" mask="url(#m1)" filter="url(#f1)">
            <a href="#top">
                <rect width="20" height="40" fill="url(#p1)"/>
            </a>
            <rect x="20" width="20" height="40" fill="url(#p2)"/>
            <use href="#tpl" x="5" y="5"/>
            <use href="#tpl" x="25" y="25"/>
            <path d="M0 20 L40 20" stroke="url(#g2)" stroke-width="2" marker-end="url(#mk1)"/>
        </g>
    </svg>"##;

    let opts = RenderOptions {
        load_system_fonts: false,
        ..Default::default()
    };
    let out = render(dag, &opts).expect("shared DAG SVG renders");
    assert_eq!((out.width, out.height), (40, 40));
    assert!(out.data.chunks(4).any(|px| px[3] > 0));

    let dec = SvgDecoderConfig::new()
        .job()
        .decoder(Cow::Borrowed(dag), &[])
        .unwrap()
        .decode()
        .expect("zencodec decode succeeds on DAG");
    assert_eq!(dec.info().width, 40);

    #[cfg(feature = "inventory")]
    {
        let inv = SvgDecoderConfig::new().job().inventory(dag).unwrap();
        assert!(inv.parts().iter().any(|p| p.disposition().is_consumed()));
    }

    // Also check valid patterns where `<use>` or `<filter>` references a sibling
    // inside `<g>` without forming a real cycle.
    let valid_use_reparent = wrap_svg(
        r##"<g fill="url(#p1)"><rect id="r1" width="4" height="4"/></g><pattern id="p1" width="4" height="4" patternUnits="userSpaceOnUse"><use href="#r1"/></pattern><rect width="20" height="20" fill="url(#p1)"/>"##,
    );
    assert!(render(&valid_use_reparent, &opts).is_ok());

    let valid_filter_in_g = wrap_svg(
        r##"<g id="g1"><filter id="f1"><feImage href="#g1"/></filter><rect width="20" height="20" fill="red"/></g><rect width="20" height="20" filter="url(#f1)"/>"##,
    );
    assert!(render(&valid_filter_in_g, &opts).is_ok());
}
