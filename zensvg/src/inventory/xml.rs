//! A byte-range XML lexer for the SVG inventory. It builds a tree of nodes
//! (declaration, DOCTYPE, comments, processing instructions, CDATA, text,
//! elements with their start tags, attributes and end tags) whose ranges
//! tile the input. It never fails: bytes it cannot place become
//! [`XKind::Malformed`] nodes. Work is linear in the input.

use std::ops::Range;

#[derive(Clone, Debug)]
pub(crate) struct XAttr {
    /// `name="value"`, quotes included.
    pub range: Range<usize>,
    pub qname: Range<usize>,
    /// Inside the quotes.
    pub value: Range<usize>,
}

#[derive(Clone, Debug)]
pub(crate) enum XKind {
    Bom,
    /// `<?xml … ?>` at the start of the document.
    Decl,
    Doctype {
        subset: bool,
        /// `SYSTEM`/`PUBLIC` literals of the document type, quotes included.
        external: Vec<Range<usize>>,
        /// Declarations, comments and parameter-entity references in the
        /// internal subset.
        items: Vec<(Range<usize>, DtdItem)>,
    },
    Pi {
        target: Range<usize>,
    },
    Comment,
    CData,
    /// Character data with at least one non-white-space byte.
    Text,
    /// White-space-only character data.
    Ws,
    Element {
        qname: Range<usize>,
        attrs: Vec<XAttr>,
        start_tag: Range<usize>,
        end_tag: Option<Range<usize>>,
        self_closing: bool,
    },
    Malformed(&'static str),
}

/// One item of a DOCTYPE internal subset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DtdItem {
    /// `<!ENTITY name …>`; `external` when it names a `SYSTEM`/`PUBLIC` id,
    /// else `value` is its literal (inside the quotes).
    Entity {
        name: Range<usize>,
        external: bool,
        value: Option<Range<usize>>,
    },
    /// `<!ELEMENT`, `<!ATTLIST`, `<!NOTATION`, a processing instruction, or
    /// a parameter-entity reference.
    Other,
    Comment,
}

#[derive(Clone, Debug)]
pub(crate) struct XNode {
    pub range: Range<usize>,
    pub kind: XKind,
    pub children: Vec<usize>,
}

pub(crate) struct XTree {
    pub nodes: Vec<XNode>,
    /// Top-level nodes, in order.
    pub roots: Vec<usize>,
}

fn is_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n')
}

fn is_name_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b == b':' || b >= 0x80
}

fn is_name(b: u8) -> bool {
    is_name_start(b) || b.is_ascii_digit() || b == b'-' || b == b'.'
}

fn find(hay: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if from > hay.len() {
        return None;
    }
    hay[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

struct Lexer<'a> {
    d: &'a [u8],
    nodes: Vec<XNode>,
    roots: Vec<usize>,
    /// Open elements, innermost last.
    stack: Vec<usize>,
}

impl Lexer<'_> {
    fn add(&mut self, range: Range<usize>, kind: XKind) -> usize {
        let id = self.nodes.len();
        self.nodes.push(XNode {
            range,
            kind,
            children: Vec::new(),
        });
        match self.stack.last() {
            Some(&p) => self.nodes[p].children.push(id),
            None => self.roots.push(id),
        }
        id
    }

    fn name_end(&self, mut i: usize) -> usize {
        while i < self.d.len() && is_name(self.d[i]) {
            i += 1;
        }
        i
    }

    /// The next `<` at or after `i`, or the end of input.
    fn next_lt(&self, i: usize) -> usize {
        self.d[i.min(self.d.len())..]
            .iter()
            .position(|&b| b == b'<')
            .map_or(self.d.len(), |p| p + i)
    }

    /// The end of a markup declaration starting at `i` (`<!…>` or `<?…?>`),
    /// skipping quoted literals.
    fn decl_end(&self, mut i: usize) -> Option<usize> {
        let d = self.d;
        if d[i..].starts_with(b"<?") {
            return find(d, i + 2, b"?>").map(|e| e + 2);
        }
        while i < d.len() {
            match d[i] {
                q @ (b'"' | b'\'') => i += d[i + 1..].iter().position(|&b| b == q)? + 2,
                b'>' => return Some(i + 1),
                _ => i += 1,
            }
        }
        None
    }

    fn doctype(&mut self, start: usize) -> usize {
        let d = self.d;
        let mut i = start + 9;
        let mut external = Vec::new();
        let mut items = Vec::new();
        let mut subset = false;
        // The head: name, then an optional external id, up to `[` or `>`.
        while i < d.len() {
            match d[i] {
                q @ (b'"' | b'\'') => {
                    let Some(e) = d[i + 1..].iter().position(|&b| b == q) else {
                        i = d.len();
                        break;
                    };
                    // The literal with its quotes, so an empty one is
                    // still a non-empty part.
                    external.push(i..i + e + 2);
                    i += e + 2;
                }
                b'[' => {
                    subset = true;
                    i += 1;
                    break;
                }
                b'>' => {
                    let end = i + 1;
                    self.add(
                        start..end,
                        XKind::Doctype {
                            subset,
                            external,
                            items,
                        },
                    );
                    return end;
                }
                _ => i += 1,
            }
        }
        // The internal subset, up to `]` and the closing `>`.
        while subset && i < d.len() {
            if is_ws(d[i]) {
                i += 1;
            } else if d[i] == b']' {
                let mut j = i + 1;
                while j < d.len() && is_ws(d[j]) {
                    j += 1;
                }
                if d.get(j) == Some(&b'>') {
                    let end = j + 1;
                    self.add(
                        start..end,
                        XKind::Doctype {
                            subset,
                            external,
                            items,
                        },
                    );
                    return end;
                }
                break;
            } else if d[i..].starts_with(b"<!--") {
                let Some(e) = find(d, i + 4, b"-->") else {
                    break;
                };
                items.push((i..e + 3, DtdItem::Comment));
                i = e + 3;
            } else if d[i..].starts_with(b"<!") || d[i..].starts_with(b"<?") {
                let Some(e) = self.decl_end(i) else {
                    break;
                };
                let item = if d[i..].starts_with(b"<!ENTITY") {
                    let mut n = i + 8;
                    while n < e && is_ws(d[n]) {
                        n += 1;
                    }
                    if d.get(n) == Some(&b'%') {
                        n += 1;
                        while n < e && is_ws(d[n]) {
                            n += 1;
                        }
                    }
                    let name = n..self.name_end(n).min(e);
                    let body = &d[name.end..e];
                    let external = body.windows(6).any(|w| w == b"SYSTEM" || w == b"PUBLIC");
                    let value = if external {
                        None
                    } else {
                        body.iter()
                            .position(|&b| b == b'"' || b == b'\'')
                            .and_then(|q| {
                                let qc = body[q];
                                let rest = &body[q + 1..];
                                rest.iter()
                                    .position(|&b| b == qc)
                                    .map(|e2| name.end + q + 1..name.end + q + 1 + e2)
                            })
                    };
                    DtdItem::Entity {
                        name,
                        external,
                        value,
                    }
                } else {
                    DtdItem::Other
                };
                items.push((i..e, item));
                i = e;
            } else if d[i] == b'%' {
                let e = d[i..]
                    .iter()
                    .position(|&b| b == b';')
                    .map_or(d.len(), |p| i + p + 1);
                items.push((i..e, DtdItem::Other));
                i = e;
            } else {
                break;
            }
        }
        let end = d.len();
        self.add(start..end, XKind::Malformed("unterminated DOCTYPE"));
        end
    }

    /// A start tag at `start` (`<` then a name start).
    fn start_tag(&mut self, start: usize) -> usize {
        let d = self.d;
        let qname = start + 1..self.name_end(start + 1);
        let mut i = qname.end;
        let mut attrs = Vec::new();
        loop {
            while i < d.len() && is_ws(d[i]) {
                i += 1;
            }
            if i >= d.len() {
                break;
            }
            if d[i..].starts_with(b"/>") {
                let end = i + 2;
                self.add(
                    start..end,
                    XKind::Element {
                        qname,
                        attrs,
                        start_tag: start..end,
                        end_tag: None,
                        self_closing: true,
                    },
                );
                return end;
            }
            if d[i] == b'>' {
                let end = i + 1;
                let id = self.add(
                    start..end,
                    XKind::Element {
                        qname,
                        attrs,
                        start_tag: start..end,
                        end_tag: None,
                        self_closing: false,
                    },
                );
                self.stack.push(id);
                return end;
            }
            if !is_name_start(d[i]) {
                break;
            }
            let a_start = i;
            let a_name = i..self.name_end(i);
            i = a_name.end;
            while i < d.len() && is_ws(d[i]) {
                i += 1;
            }
            if d.get(i) != Some(&b'=') {
                break;
            }
            i += 1;
            while i < d.len() && is_ws(d[i]) {
                i += 1;
            }
            let Some(&q @ (b'"' | b'\'')) = d.get(i) else {
                break;
            };
            let Some(e) = d[i + 1..].iter().position(|&b| b == q) else {
                break;
            };
            let value = i + 1..i + 1 + e;
            i = value.end + 1;
            attrs.push(XAttr {
                range: a_start..i,
                qname: a_name,
                value,
            });
        }
        // A broken start tag: up to the next `<` is malformed.
        let end = self.next_lt(i.max(start + 1));
        self.add(start..end, XKind::Malformed("malformed start tag"));
        end
    }

    fn end_tag(&mut self, start: usize) -> usize {
        let d = self.d;
        let name = start + 2..self.name_end(start + 2);
        let mut i = name.end;
        while i < d.len() && is_ws(d[i]) {
            i += 1;
        }
        if d.get(i) != Some(&b'>') {
            let end = self.next_lt(i.max(start + 2));
            self.add(start..end, XKind::Malformed("malformed end tag"));
            return end;
        }
        let end = i + 1;
        let matches = |n: &XNode| match &n.kind {
            XKind::Element { qname, .. } => d[qname.clone()] == d[name.clone()],
            _ => false,
        };
        match self.stack.iter().rposition(|&e| matches(&self.nodes[e])) {
            Some(depth) => {
                // Elements opened inside it and never closed end here.
                while self.stack.len() > depth + 1 {
                    let open = self.stack.pop().unwrap_or_default();
                    self.nodes[open].range.end = start;
                }
                let el = self.stack.pop().unwrap_or_default();
                self.nodes[el].range.end = end;
                if let XKind::Element { end_tag, .. } = &mut self.nodes[el].kind {
                    *end_tag = Some(start..end);
                }
            }
            None => {
                self.add(start..end, XKind::Malformed("end tag without a start tag"));
            }
        }
        end
    }

    fn run(mut self) -> XTree {
        let d = self.d;
        let mut i = 0;
        if d.starts_with(&[0xEF, 0xBB, 0xBF]) {
            self.add(0..3, XKind::Bom);
            i = 3;
        }
        while i < d.len() {
            let next = if d[i] != b'<' {
                let e = self.next_lt(i);
                let kind = if d[i..e].iter().all(|&b| is_ws(b)) {
                    XKind::Ws
                } else {
                    XKind::Text
                };
                self.add(i..e, kind);
                e
            } else if d[i..].starts_with(b"<?") {
                match find(d, i + 2, b"?>") {
                    Some(e) => {
                        let target = i + 2..self.name_end(i + 2);
                        let at_start = self.nodes.iter().all(|n| matches!(n.kind, XKind::Bom));
                        let kind = if at_start && &d[target.clone()] == b"xml" {
                            XKind::Decl
                        } else {
                            XKind::Pi { target }
                        };
                        self.add(i..e + 2, kind);
                        e + 2
                    }
                    None => {
                        self.add(
                            i..d.len(),
                            XKind::Malformed("unterminated processing instruction"),
                        );
                        d.len()
                    }
                }
            } else if d[i..].starts_with(b"<!--") {
                match find(d, i + 4, b"-->") {
                    Some(e) => {
                        self.add(i..e + 3, XKind::Comment);
                        e + 3
                    }
                    None => {
                        self.add(i..d.len(), XKind::Malformed("unterminated comment"));
                        d.len()
                    }
                }
            } else if d[i..].starts_with(b"<![CDATA[") {
                match find(d, i + 9, b"]]>") {
                    Some(e) => {
                        self.add(i..e + 3, XKind::CData);
                        e + 3
                    }
                    None => {
                        self.add(i..d.len(), XKind::Malformed("unterminated CDATA section"));
                        d.len()
                    }
                }
            } else if d[i..].starts_with(b"<!DOCTYPE") {
                self.doctype(i)
            } else if d[i..].starts_with(b"</") {
                self.end_tag(i)
            } else if d.get(i + 1).is_some_and(|&b| is_name_start(b)) {
                self.start_tag(i)
            } else {
                let e = self.next_lt(i + 1);
                self.add(i..e, XKind::Malformed("stray '<'"));
                e
            };
            i = next.max(i + 1);
        }
        // Elements still open at the end of input run to it.
        for open in std::mem::take(&mut self.stack) {
            self.nodes[open].range.end = d.len();
        }
        XTree {
            nodes: self.nodes,
            roots: self.roots,
        }
    }
}

/// roxmltree's deepest chain of nested entity references
/// (`LoopDetector::inc_depth`: a reference at depth 10 is an
/// `EntityReferenceLoop`).
const MAX_ENTITY_CHAIN: usize = 10;

/// An upper bound on how deep roxmltree nests elements when it parses `d`:
/// the lexer's element nesting, plus, at every entity reference in
/// character data, the nesting the referenced internal entity expands into
/// (its literal's own elements and the entities it references in turn).
/// `None` when a referenced entity chains deeper than roxmltree allows or
/// loops (roxmltree rejects the document).
///
/// roxmltree recurses once per nested element (`parse_element` ↔
/// `parse_content`), so this is checked before roxmltree runs: a deep
/// document would overflow the stack and abort the process.
pub(crate) fn nesting_bound(d: &[u8], tree: &XTree) -> Option<usize> {
    let mut entities: Vec<(&[u8], &[u8])> = Vec::new();
    for &r in &tree.roots {
        if let XKind::Doctype { items, .. } = &tree.nodes[r].kind {
            for (_, item) in items {
                if let DtdItem::Entity {
                    name,
                    value: Some(v),
                    ..
                } = item
                {
                    // roxmltree keeps the first declaration of a name.
                    if !entities.iter().any(|(n, _)| *n == &d[name.clone()]) {
                        entities.push((&d[name.clone()], &d[v.clone()]));
                    }
                }
            }
        }
    }
    let depths = entity_depths(&entities);
    let lookup = |name: &[u8]| -> Option<Option<usize>> {
        entities
            .iter()
            .position(|(n, _)| *n == name)
            .map(|i| depths[i])
    };
    let mut best = 0usize;
    let mut stack: Vec<(usize, usize)> = tree.roots.iter().map(|&r| (r, 0)).collect();
    while let Some((n, depth)) = stack.pop() {
        match &tree.nodes[n].kind {
            XKind::Element { .. } => {
                best = best.max(depth + 1);
                stack.extend(tree.nodes[n].children.iter().map(|&c| (c, depth + 1)));
            }
            XKind::Text if depth > 0 => {
                for name in references(&d[tree.nodes[n].range.clone()]) {
                    match lookup(name) {
                        Some(Some(e)) => best = best.max(depth + e),
                        Some(None) => return None,
                        None => {}
                    }
                }
            }
            _ => {}
        }
    }
    Some(best)
}

/// Names of the general entity references (`&name;`) in `v`, excluding
/// character references.
fn references(v: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut i = 0;
    std::iter::from_fn(move || {
        while i < v.len() {
            if v[i] == b'&'
                && let Some(semi) = v[i + 1..].iter().position(|&b| b == b';')
            {
                let name = &v[i + 1..i + 1 + semi];
                i += semi + 2;
                if !name.is_empty() && name[0] != b'#' && name.iter().all(|&b| is_name(b)) {
                    return Some(name);
                }
                continue;
            }
            i += 1;
        }
        None
    })
}

/// For each entity, the element nesting its replacement text expands into,
/// or `None` when it chains deeper than roxmltree allows or loops. Iterative
/// (the chain length is not bounded by anything but the input).
fn entity_depths(entities: &[(&[u8], &[u8])]) -> Vec<Option<usize>> {
    // Per entity: (references with the element depth they sit at, own
    // element nesting).
    let index = |name: &[u8]| entities.iter().position(|(n, _)| *n == name);
    let shape: Vec<(Vec<(usize, usize)>, usize)> = entities
        .iter()
        .map(|(_, v)| {
            let (mut depth, mut own, mut refs) = (0usize, 0usize, Vec::new());
            let mut i = 0;
            while i < v.len() {
                match v[i] {
                    b'<' if v.get(i + 1) == Some(&b'/') => depth = depth.saturating_sub(1),
                    b'<' if v.get(i + 1).is_some_and(|&b| is_name_start(b)) => {
                        own = own.max(depth + 1);
                        // The tag's `>`, skipping quoted attribute values;
                        // an unterminated tag counts as open (an upper bound).
                        let mut c = i + 1;
                        let mut quote = None;
                        while c < v.len() {
                            match (quote, v[c]) {
                                (None, q @ (b'"' | b'\'')) => quote = Some(q),
                                (Some(q), b) if b == q => quote = None,
                                (None, b'>') => break,
                                _ => {}
                            }
                            c += 1;
                        }
                        let self_closing = c < v.len() && v[c - 1] == b'/';
                        if !self_closing {
                            depth += 1;
                        }
                    }
                    b'&' => {
                        if let Some(name) = references(&v[i..]).next()
                            && v[i + 1..].starts_with(name)
                            && let Some(e) = index(name)
                        {
                            refs.push((e, depth));
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            (refs, own)
        })
        .collect();
    // (depth, chain height) once known; `None` = rejected.
    let mut done: Vec<Option<Option<(usize, usize)>>> = vec![None; entities.len()];
    let mut on_path = vec![false; entities.len()];
    for start in 0..entities.len() {
        if done[start].is_some() {
            continue;
        }
        let mut stack = vec![(start, 0usize)];
        on_path[start] = true;
        while let Some(&mut (e, ref mut next)) = stack.last_mut() {
            if let Some(&(child, _)) = shape[e].0.get(*next) {
                *next += 1;
                if on_path[child] {
                    // A loop: roxmltree fails with EntityReferenceLoop.
                    done[child] = Some(None);
                } else if done[child].is_none() {
                    on_path[child] = true;
                    stack.push((child, 0));
                }
                continue;
            }
            // All references of `e` are resolved.
            let mut result = Some((shape[e].1, 1usize));
            for &(child, at) in &shape[e].0 {
                result = match (result, done[child].flatten()) {
                    (Some((d, h)), Some((cd, ch))) => Some((d.max(at + cd), h.max(ch + 1))),
                    _ => None,
                };
            }
            if result.is_some_and(|(_, h)| h > MAX_ENTITY_CHAIN) {
                result = None;
            }
            if done[e] != Some(None) {
                done[e] = Some(result);
            }
            on_path[e] = false;
            stack.pop();
        }
    }
    done.into_iter()
        .map(|r| r.flatten().map(|(d, _)| d))
        .collect()
}

pub(crate) fn lex(d: &[u8]) -> XTree {
    Lexer {
        d,
        nodes: Vec::new(),
        roots: Vec::new(),
        stack: Vec::new(),
    }
    .run()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiles(t: &XTree, kids: &[usize], span: Range<usize>) {
        let mut at = span.start;
        for &k in kids {
            assert_eq!(t.nodes[k].range.start, at, "{:?}", t.nodes[k]);
            at = t.nodes[k].range.end;
        }
        assert_eq!(at, span.end);
    }

    #[test]
    fn nodes_tile_the_document() {
        let d = br#"<?xml version="1.0"?>
<!DOCTYPE svg [ <!ENTITY a "b"> ]>
<!-- c --><svg a="1" b='2'><g><![CDATA[x]]>t<path/></g></svg>
"#;
        let t = lex(d);
        tiles(&t, &t.roots, 0..d.len());
        let svg = t
            .roots
            .iter()
            .copied()
            .find(|&r| matches!(t.nodes[r].kind, XKind::Element { .. }))
            .unwrap();
        let XKind::Element {
            attrs,
            start_tag,
            end_tag,
            ..
        } = &t.nodes[svg].kind
        else {
            panic!()
        };
        assert_eq!(attrs.len(), 2);
        assert_eq!(&d[attrs[1].value.clone()], b"2");
        tiles(
            &t,
            &t.nodes[svg].children,
            start_tag.end..end_tag.clone().unwrap().start,
        );
        let XKind::Doctype { subset, items, .. } = &t.nodes[t.roots[2]].kind else {
            panic!("{:?}", t.nodes[t.roots[2]])
        };
        assert!(*subset);
        assert_eq!(items.len(), 1);
        let DtdItem::Entity {
            external: false,
            value: Some(v),
            ..
        } = &items[0].1
        else {
            panic!("{items:?}")
        };
        assert_eq!(&d[v.clone()], b"b");
    }

    #[test]
    fn unclosed_elements_run_to_the_end() {
        let d = b"<svg><g><rect>";
        let t = lex(d);
        assert_eq!(t.roots.len(), 1);
        assert_eq!(t.nodes[t.roots[0]].range, 0..d.len());
    }

    #[test]
    fn broken_tags_become_malformed() {
        let d = b"<svg><g a=\"1></svg>";
        let t = lex(d);
        tiles(&t, &t.roots, 0..d.len());
    }
}
