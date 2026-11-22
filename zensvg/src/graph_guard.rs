//! O(V + E) cycle and `href`-loop guard on a parsed [`roxmltree::Document`]
//! before passing it to `usvg::Tree::from_xmltree`.
//!
//! `usvg` 0.49 breaks only length-1 and length-2 cycles for a subset of element
//! kinds, leaves `HrefIter` unguarded against lasso loops that revisit an
//! element other than the origin (`#q1 -> #q2 -> #q3 -> #q2`), and overflows the
//! process stack on cycles of length >= 3 or cross-type cycles. This module
//! builds the document's `id` and containment graph once in O(V + E) and rejects
//! any `href`/`xlink:href` cycle or paint/resource/container cycle of length >= 1
//! with [`SvgError::Parse`] while permitting shared non-cyclic `<defs>` DAGs.

use std::collections::HashMap;

use usvg::roxmltree as rx;

use crate::error::SvgError;
use crate::inventory::model::{XmlNode, parse_tag_name};

const SVG_NS: &str = "http://www.w3.org/2000/svg";
const XLINK_NS: &str = "http://www.w3.org/1999/xlink";
const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";

/// Maximum non-cyclic resource reference hops (`url(#...)`, `href`, `<use>`)
/// before rejecting to keep `usvg`'s recursive converter from overflowing the
/// stack on pathological long chains.
const MAX_RESOURCE_DEPTH: usize = 512;

/// Indices into the 8 tracked link-bearing presentation properties.
const ATTR_FILL: usize = 0;
const ATTR_STROKE: usize = 1;
const ATTR_MARKER_START: usize = 2;
const ATTR_MARKER_MID: usize = 3;
const ATTR_MARKER_END: usize = 4;
const ATTR_FILTER: usize = 5;
const ATTR_CLIP_PATH: usize = 6;
const ATTR_MASK: usize = 7;

fn link_attr_slot(name: &str) -> Option<usize> {
    match name {
        "fill" => Some(ATTR_FILL),
        "stroke" => Some(ATTR_STROKE),
        "marker-start" => Some(ATTR_MARKER_START),
        "marker-mid" => Some(ATTR_MARKER_MID),
        "marker-end" => Some(ATTR_MARKER_END),
        "filter" => Some(ATTR_FILTER),
        "clip-path" => Some(ATTR_CLIP_PATH),
        "mask" => Some(ATTR_MASK),
        _ => None,
    }
}

fn is_renderable_child(tag: &str) -> bool {
    matches!(
        tag,
        "rect"
            | "circle"
            | "ellipse"
            | "line"
            | "polyline"
            | "polygon"
            | "path"
            | "image"
            | "text"
            | "use"
            | "g"
            | "a"
            | "switch"
            | "svg"
    )
}

fn is_graphic_tag(tag: &str) -> bool {
    matches!(
        tag,
        "rect"
            | "circle"
            | "ellipse"
            | "line"
            | "polyline"
            | "polygon"
            | "path"
            | "image"
            | "text"
            | "use"
    )
}

fn is_paint_server_tag(tag: Option<&str>) -> bool {
    matches!(tag, Some("linearGradient" | "radialGradient" | "pattern"))
}

fn is_resource_container_tag(tag: &str) -> bool {
    matches!(tag, "pattern" | "mask" | "marker" | "clipPath" | "filter")
}

fn is_render_container_tag(tag: &str) -> bool {
    matches!(
        tag,
        "svg" | "g" | "a" | "switch" | "symbol" | "defs" | "pattern" | "mask" | "marker"
    )
}

fn is_skipped_subtree_tag(node: rx::Node<'_, '_>) -> bool {
    parse_tag_name(node).is_none_or(|t| t == "style")
}

/// Extract the winning `href` / `xlink:href` attribute value per SVG 2 / `usvg`
/// precedence (unprefixed `href` overrides `xlink:href`).
fn raw_href<'a>(node: rx::Node<'a, '_>) -> Option<&'a str> {
    node.attributes()
        .find(|a| a.name() == "href" && a.namespace().is_none())
        .or_else(|| {
            node.attributes()
                .find(|a| a.name() == "href" && a.namespace() == Some(XLINK_NS))
        })
        .map(|a| a.value())
}

fn svg_id_attr<'a>(node: rx::Node<'a, '_>) -> Option<&'a str> {
    node.attributes()
        .find(|a| {
            a.name() == "id"
                && matches!(
                    a.namespace(),
                    None | Some(SVG_NS) | Some(XLINK_NS) | Some(XML_NS)
                )
        })
        .map(|a| a.value())
}

fn node_label(doc: &rx::Document<'_>, idx: usize) -> String {
    let Some(raw) = u32::try_from(idx).ok().map(rx::NodeId::new) else {
        return format!("node#{idx}");
    };
    doc.get_node(raw)
        .and_then(|n| n.attribute("id"))
        .map(|id| format!("#{id}"))
        .unwrap_or_else(|| {
            doc.get_node(raw)
                .map(|n| format!("<{}>", n.tag_name().name()))
                .unwrap_or_else(|| format!("node#{idx}"))
        })
}

#[derive(Clone, Copy)]
struct AttrVal<'a> {
    value: &'a str,
    important: bool,
}

fn apply_decl<'a>(
    vals: &mut [Option<AttrVal<'a>>; 8],
    name: &str,
    value: &'a str,
    important: bool,
) {
    if name == "marker" {
        for slot in [ATTR_MARKER_START, ATTR_MARKER_MID, ATTR_MARKER_END] {
            if !vals[slot].is_some_and(|cur| cur.important) {
                vals[slot] = Some(AttrVal { value, important });
            }
        }
    } else if let Some(slot) = link_attr_slot(name)
        && !vals[slot].is_some_and(|cur| cur.important)
    {
        vals[slot] = Some(AttrVal { value, important });
    }
}

fn resolve_slot_targets<'a>(
    slot: usize,
    val: &'a str,
    last_id_map: &HashMap<&'a str, usize>,
    tags: &[Option<&'static str>],
    out: &mut Vec<(usize, bool)>,
) {
    match slot {
        ATTR_FILL | ATTR_STROKE => {
            if let Ok(svgtypes::Paint::FuncIRI(id, _)) = svgtypes::Paint::from_str(val)
                && let Some(&target) = last_id_map.get(id)
                && is_paint_server_tag(tags[target])
            {
                out.push((target, true));
            }
        }
        ATTR_MARKER_START | ATTR_MARKER_MID | ATTR_MARKER_END => {
            if let Ok(svgtypes::FuncIRI(id)) = svgtypes::FuncIRI::from_str(val)
                && let Some(&target) = last_id_map.get(id)
                && tags[target] == Some("marker")
            {
                out.push((target, true));
            }
        }
        ATTR_FILTER => {
            for item in svgtypes::FilterValueListParser::from(val) {
                if let Ok(svgtypes::FilterValue::Url(id)) = item
                    && let Some(&target) = last_id_map.get(id)
                    && tags[target] == Some("filter")
                {
                    out.push((target, true));
                }
            }
        }
        ATTR_CLIP_PATH => {
            if let Ok(svgtypes::FuncIRI(id)) = svgtypes::FuncIRI::from_str(val)
                && let Some(&target) = last_id_map.get(id)
                && tags[target] == Some("clipPath")
            {
                out.push((target, true));
            }
        }
        ATTR_MASK => {
            if let Ok(svgtypes::FuncIRI(id)) = svgtypes::FuncIRI::from_str(val)
                && let Some(&target) = last_id_map.get(id)
                && tags[target] == Some("mask")
            {
                out.push((target, true));
            }
        }
        _ => {}
    }
}

/// Validate that `doc` contains no `href`/`xlink:href` cycle (including
/// lasso/stem chains) and no paint/resource/container/inheritance cycle of
/// length `>= 1`, in O(V + E) time and heap space without recursion.
pub(crate) fn validate_acyclic(doc: &rx::Document<'_>) -> Result<(), SvgError> {
    let num_nodes = doc
        .descendants()
        .map(|n| n.id().get_usize() + 1)
        .max()
        .unwrap_or(0);
    if num_nodes == 0 {
        return Ok(());
    }

    let mut tags: Vec<Option<&'static str>> = vec![None; num_nodes];
    let mut first_id_map: HashMap<&str, usize> = HashMap::new();
    for node in doc.descendants() {
        let idx = node.id().get_usize();
        if node.is_element() {
            tags[idx] = parse_tag_name(node);
            if let Some(id) = node.attribute("id") {
                first_id_map.entry(id).or_insert(idx);
            }
        }
    }

    // Build `last_id_map` over `svgtree`-reachable elements (matching `usvg`'s
    // `doc.links`: skipped subtrees like `<style>`, `<metadata>`, foreign
    // namespaces are excluded).
    let mut last_id_map: HashMap<&str, usize> = HashMap::new();
    {
        let mut stack: Vec<rx::Node<'_, '_>> = Vec::new();
        for child in doc.root().children().rev() {
            stack.push(child);
        }
        while let Some(node) = stack.pop() {
            if is_skipped_subtree_tag(node) {
                continue;
            }
            if let Some(id) = svg_id_attr(node) {
                last_id_map.insert(id, node.id().get_usize());
            }
            // Push in reverse so children are visited in document order (last wins).
            let kids: Vec<_> = node.children().collect();
            for c in kids.into_iter().rev() {
                stack.push(c);
            }
        }
    }

    // Parse `<style>` CSS rules (if any).
    let mut sheet = simplecss::StyleSheet::new();
    for node in doc.descendants() {
        if node.is_element()
            && node.tag_name().name() == "style"
            && matches!(node.attribute("type"), None | Some("text/css"))
            && let Some(text) = node.text()
        {
            sheet.parse_more(text);
        }
    }

    // Per-node outgoing functional `href` edge for Check A (all `href`/`xlink:href`
    // chains excluding `<a>` hyperlinks and `<tspan>`).
    let mut href_next: Vec<Option<usize>> = vec![None; num_nodes];

    // Adjacency lists for Check B: `(target_idx, is_reference_hop)`.
    let mut adj: Vec<Vec<(usize, bool)>> = vec![Vec::new(); num_nodes];
    let mut fe_image_targets: Vec<usize> = Vec::new();
    let mut deferred_inherited: Vec<Option<[Option<&str>; 5]>> = vec![None; num_nodes];

    // Pre-order iterative DOM walk to resolve CSS/style/attributes, `"inherit"`,
    // resource containment, and `<feImage>` target inheritance.
    enum Frame<'a, 'input> {
        Enter {
            node: rx::Node<'a, 'input>,
            in_res: bool,
        },
        Exit {
            pushed_inheritable: u8,
            pushed_non_inheritable: bool,
        },
    }

    let mut inherited_stack: [Vec<&str>; 5] = Default::default();
    let mut non_inheritable_stack: Vec<[Option<&str>; 3]> = Vec::new();

    let mut dom_stack: Vec<Frame<'_, '_>> = Vec::new();
    for child in doc.root().children().rev() {
        if child.is_element() {
            dom_stack.push(Frame::Enter {
                node: child,
                in_res: false,
            });
        }
    }

    while let Some(frame) = dom_stack.pop() {
        match frame {
            Frame::Exit {
                pushed_inheritable,
                pushed_non_inheritable,
            } => {
                for (k, stack) in inherited_stack.iter_mut().enumerate() {
                    if (pushed_inheritable & (1 << k)) != 0 {
                        stack.pop();
                    }
                }
                if pushed_non_inheritable {
                    non_inheritable_stack.pop();
                }
            }
            Frame::Enter { node, in_res } => {
                let u = node.id().get_usize();
                let tag = tags[u];

                let mut vals: [Option<AttrVal<'_>>; 8] = [None; 8];
                for attr in node.attributes() {
                    if !matches!(
                        attr.namespace(),
                        None | Some(SVG_NS) | Some(XLINK_NS) | Some(XML_NS)
                    ) {
                        continue;
                    }
                    if let Some(slot) = link_attr_slot(attr.name()) {
                        vals[slot] = Some(AttrVal {
                            value: attr.value(),
                            important: false,
                        });
                    }
                }
                if !sheet.rules.is_empty() {
                    let xml_node = XmlNode(node);
                    for rule in &sheet.rules {
                        if rule.selector.matches(&xml_node) {
                            for decl in &rule.declarations {
                                apply_decl(&mut vals, decl.name, decl.value, decl.important);
                            }
                        }
                    }
                }
                if let Some(style) = node.attribute("style") {
                    for decl in simplecss::DeclarationTokenizer::from(style) {
                        apply_decl(&mut vals, decl.name, decl.value, decl.important);
                    }
                }

                // Resolve `"inherit"` against ancestors/direct parent.
                let parent_non_inh = non_inheritable_stack.last().copied().unwrap_or([None; 3]);
                let mut resolved_strs: [Option<&str>; 8] = [None; 8];
                for slot in 0..8 {
                    if let Some(AttrVal { value, .. }) = vals[slot] {
                        if value == "inherit" {
                            if slot < 5 {
                                resolved_strs[slot] = inherited_stack[slot].last().copied();
                            } else {
                                resolved_strs[slot] = parent_non_inh[slot - 5];
                            }
                        } else {
                            resolved_strs[slot] = Some(value);
                        }
                    }
                }

                // Track `href` / `xlink:href` edges (excluding `<a>` and `<tspan>`).
                if !matches!(tag, Some("a" | "tspan"))
                    && let Some(href_val) = raw_href(node)
                    && let Ok(svgtypes::IRI(id)) = svgtypes::IRI::from_str(href_val)
                {
                    let target = if matches!(tag, Some("use" | "tref")) {
                        first_id_map.get(id).copied()
                    } else {
                        last_id_map.get(id).copied()
                    };
                    if let Some(target_idx) = target {
                        href_next[u] = Some(target_idx);
                        if tag == Some("feImage") {
                            fe_image_targets.push(target_idx);
                        }
                        if matches!(
                            tag,
                            Some(
                                "use"
                                    | "feImage"
                                    | "pattern"
                                    | "linearGradient"
                                    | "radialGradient"
                                    | "filter"
                            )
                        ) {
                            adj[u].push((target_idx, true));
                        }
                    }
                }

                // Resolve own `url(#id)` edges for `u`.
                let mut has_own_paint_or_marker = [false; 5];
                for (slot, &opt_val) in resolved_strs.iter().enumerate() {
                    if let Some(val) = opt_val {
                        if slot < 5 {
                            has_own_paint_or_marker[slot] = true;
                        }
                        resolve_slot_targets(slot, val, &last_id_map, &tags, &mut adj[u]);
                    }
                }

                // Inherited paint/marker attributes: active inside resource
                // containers (`pattern`, `mask`, `marker`, `clipPath`, `filter`)
                // or deferred for potential `<feImage>` target subtrees.
                if let Some(t) = tag
                    && (is_graphic_tag(t) || t == "tspan")
                {
                    if in_res {
                        for (slot, stack) in inherited_stack.iter().enumerate() {
                            if !has_own_paint_or_marker[slot]
                                && let Some(&inh_val) = stack.last()
                            {
                                resolve_slot_targets(
                                    slot,
                                    inh_val,
                                    &last_id_map,
                                    &tags,
                                    &mut adj[u],
                                );
                            }
                        }
                    } else {
                        let mut any_inh = false;
                        let mut inh_vals: [Option<&str>; 5] = [None; 5];
                        for (slot, stack) in inherited_stack.iter().enumerate() {
                            if !has_own_paint_or_marker[slot]
                                && let Some(&inh_val) = stack.last()
                            {
                                inh_vals[slot] = Some(inh_val);
                                any_inh = true;
                            }
                        }
                        if any_inh {
                            deferred_inherited[u] = Some(inh_vals);
                        }
                    }
                }

                // Add container nesting edges to children that `usvg` actually
                // converts under `u`.
                let child_in_res = match tag {
                    Some(t) if is_resource_container_tag(t) => true,
                    None | Some("style") => false,
                    _ => in_res,
                };
                for child in node.children() {
                    if !child.is_element() {
                        continue;
                    }
                    let c_idx = child.id().get_usize();
                    let c_tag = tags[c_idx];
                    let add_edge = match (tag, c_tag) {
                        (Some(pt), Some(ct)) if is_render_container_tag(pt) => {
                            is_renderable_child(ct)
                        }
                        (Some("clipPath"), Some(ct)) => is_graphic_tag(ct),
                        (Some("filter"), Some(ct)) => ct.starts_with("fe"),
                        (Some("text" | "tspan" | "textPath" | "a"), Some(ct)) => {
                            matches!(ct, "tspan" | "tref" | "textPath" | "a")
                        }
                        _ => false,
                    };
                    if add_edge {
                        let ref_hop = tag.is_some_and(is_resource_container_tag);
                        adj[u].push((c_idx, ref_hop));
                    }
                }

                // Push Exit frame then element children in reverse order.
                let mut pushed_inh_mask = 0u8;
                for (slot, opt_val) in resolved_strs[..5].iter().enumerate() {
                    if let Some(val) = opt_val {
                        inherited_stack[slot].push(val);
                        pushed_inh_mask |= 1 << slot;
                    }
                }
                let next_non_inh = [
                    resolved_strs[ATTR_FILTER],
                    resolved_strs[ATTR_CLIP_PATH],
                    resolved_strs[ATTR_MASK],
                ];
                non_inheritable_stack.push(next_non_inh);
                dom_stack.push(Frame::Exit {
                    pushed_inheritable: pushed_inh_mask,
                    pushed_non_inheritable: true,
                });
                let children: Vec<_> = node.children().filter(|c| c.is_element()).collect();
                for child in children.into_iter().rev() {
                    dom_stack.push(Frame::Enter {
                        node: child,
                        in_res: child_in_res,
                    });
                }
            }
        }
    }

    // Propagate deferred inherited paint/marker edges onto any subtree targeted
    // by `<feImage href="#...">`, since `usvg` converts `feImage` targets in
    // their original DOM ancestor chain without reparenting.
    if !fe_image_targets.is_empty() {
        let mut fe_reachable = vec![false; num_nodes];
        let mut stack = fe_image_targets;
        while let Some(u) = stack.pop() {
            if fe_reachable[u] {
                continue;
            }
            fe_reachable[u] = true;
            if let Some(inh_vals) = deferred_inherited[u].take() {
                for (slot, opt_val) in inh_vals.iter().enumerate() {
                    if let Some(val) = opt_val {
                        resolve_slot_targets(slot, val, &last_id_map, &tags, &mut adj[u]);
                    }
                }
            }
            if let Some(node) = u32::try_from(u)
                .ok()
                .and_then(|id| doc.get_node(rx::NodeId::new(id)))
            {
                for child in node.children() {
                    if child.is_element() {
                        stack.push(child.id().get_usize());
                    }
                }
            }
        }
    }

    // Check A: O(V) iterative 3-color cycle detection on the functional `href`
    // graph (rejects self-loops, 2-cycles, 3+-cycles, and lasso/stem loops).
    {
        let mut color = vec![0u8; num_nodes];
        let mut path = Vec::new();
        for start in 0..num_nodes {
            if color[start] != 0 || href_next[start].is_none() {
                continue;
            }
            path.clear();
            let mut cur = Some(start);
            while let Some(u) = cur {
                match color[u] {
                    0 => {
                        color[u] = 1;
                        path.push(u);
                        cur = href_next[u];
                    }
                    1 => {
                        return Err(SvgError::Parse(format!(
                            "cyclic SVG href reference involving {}",
                            node_label(doc, u)
                        )));
                    }
                    _ => break,
                }
            }
            for &u in &path {
                color[u] = 2;
            }
        }
    }

    // Check B: O(V + E) iterative 3-color DFS on the full resource / paint /
    // container / inheritance graph, also bounding longest reference chain
    // depth in the DAG.
    {
        let mut color = vec![0u8; num_nodes];
        let mut max_depth = vec![0usize; num_nodes];
        let mut stack: Vec<(usize, usize)> = Vec::new();
        for start in 0..num_nodes {
            if color[start] != 0 || adj[start].is_empty() {
                continue;
            }
            color[start] = 1;
            stack.push((start, 0));
            while let Some(&mut (u, ref mut ei)) = stack.last_mut() {
                if *ei < adj[u].len() {
                    let (v, is_ref) = adj[u][*ei];
                    *ei += 1;
                    match color[v] {
                        0 => {
                            color[v] = 1;
                            stack.push((v, 0));
                        }
                        1 => {
                            return Err(SvgError::Parse(format!(
                                "cyclic SVG resource reference involving {}",
                                node_label(doc, v)
                            )));
                        }
                        _ => {
                            let cand = max_depth[v].saturating_add(usize::from(is_ref));
                            if cand > max_depth[u] {
                                max_depth[u] = cand;
                            }
                        }
                    }
                } else {
                    color[u] = 2;
                    let u_depth = max_depth[u];
                    if u_depth > MAX_RESOURCE_DEPTH {
                        return Err(SvgError::Parse(format!(
                            "SVG resource reference depth exceeds limit ({MAX_RESOURCE_DEPTH}) at {}",
                            node_label(doc, u)
                        )));
                    }
                    stack.pop();
                    if let Some(&(parent, parent_ei)) = stack.last() {
                        let is_ref = adj[parent][parent_ei - 1].1;
                        let cand = u_depth.saturating_add(usize::from(is_ref));
                        if cand > max_depth[parent] {
                            max_depth[parent] = cand;
                        }
                    }
                }
            }
        }
    }

    Ok(())
}
