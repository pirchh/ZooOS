//! Window layouts as a small split tree (docs/layouts-design.md): one
//! general arrangement the window server can make, so "rows", "quarters",
//! "two on top and one below" or "Notes big on the left" are descriptions,
//! not new code each. Bella turns words into a tree; the window server
//! fills its leaves with windows and places them.
//!
//! The text form: `h(...)` side by side, `v(...)` stacked, children
//! separated by commas, each optionally weighted (`notes:2`). A leaf is an
//! app's name, `*` (the next window: named apps are taken first, then the
//! windows in front) or `...` (every remaining window in front).
//! Examples: `h(files,notes)`, `v(files,notes)`, `v(h(*,*),*)`,
//! `h(notes:2,v(files,network))`.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// Most windows one layout places, and most nesting.
pub const MAX_LEAVES: usize = 6;
const MAX_DEPTH: usize = 3;
const MAX_WEIGHT: u32 = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Leaf {
    /// An app by name (lower case, as the window server knows it).
    App(String),
    /// The next window.
    Next,
    /// Every remaining window.
    Rest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    Leaf(Leaf),
    /// Children side by side (`h`) or stacked (`v`), each with a weight.
    Split {
        across: bool,
        parts: Vec<(Node, u32)>,
    },
}

impl Node {
    /// Leaves in order (left to right, top to bottom).
    pub fn leaves(&self) -> Vec<&Leaf> {
        match self {
            Node::Leaf(l) => alloc::vec![l],
            Node::Split { parts, .. } => parts.iter().flat_map(|(n, _)| n.leaves()).collect(),
        }
    }

    /// The text form (the parser's input).
    pub fn text(&self) -> String {
        match self {
            Node::Leaf(Leaf::App(a)) => a.clone(),
            Node::Leaf(Leaf::Next) => String::from("*"),
            Node::Leaf(Leaf::Rest) => String::from("..."),
            Node::Split { across, parts } => {
                let inner: Vec<String> = parts
                    .iter()
                    .map(|(n, w)| {
                        if *w == 1 {
                            n.text()
                        } else {
                            format!("{}:{w}", n.text())
                        }
                    })
                    .collect();
                format!("{}({})", if *across { "h" } else { "v" }, inner.join(","))
            }
        }
    }

    /// `...` replaced by `n` copies of `*`, so the tree has exact leaves.
    pub fn expand_rest(&self, n: usize) -> Node {
        match self {
            Node::Leaf(_) => self.clone(),
            Node::Split { across, parts } => {
                let mut out = Vec::new();
                for (child, w) in parts {
                    if *child == Node::Leaf(Leaf::Rest) {
                        out.extend((0..n).map(|_| (Node::Leaf(Leaf::Next), *w)));
                    } else {
                        out.push((child.expand_rest(n), *w));
                    }
                }
                Node::Split {
                    across: *across,
                    parts: out,
                }
            }
        }
    }
}

/// Parses the text form; `None` for anything malformed, too deep, or with
/// more than `MAX_LEAVES` leaves (`...` counts as one until expanded).
pub fn parse(text: &str) -> Option<Node> {
    let t: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let mut at = 0;
    let node = node(t.as_bytes(), &mut at, 0)?;
    (at == t.len() && node.leaves().len() <= MAX_LEAVES).then_some(node)
}

fn node(b: &[u8], at: &mut usize, depth: usize) -> Option<Node> {
    if depth > MAX_DEPTH {
        return None;
    }
    if (b.get(*at) == Some(&b'h') || b.get(*at) == Some(&b'v')) && b.get(*at + 1) == Some(&b'(') {
        let across = b[*at] == b'h';
        *at += 2;
        let mut parts = Vec::new();
        loop {
            let child = node(b, at, depth + 1)?;
            let mut weight = 1;
            if b.get(*at) == Some(&b':') {
                *at += 1;
                let d = *b.get(*at)?;
                if !d.is_ascii_digit() {
                    return None;
                }
                weight = u32::from(d - b'0');
                *at += 1;
                if weight == 0 || weight > MAX_WEIGHT {
                    return None;
                }
            }
            parts.push((child, weight));
            match b.get(*at) {
                Some(b',') => *at += 1,
                Some(b')') => {
                    *at += 1;
                    break;
                }
                _ => return None,
            }
        }
        // Two parts at least, or `...` alone ("every window, stacked").
        let ok = parts.len() >= 2 || (parts.len() == 1 && parts[0].0 == Node::Leaf(Leaf::Rest));
        return ok.then_some(Node::Split { across, parts });
    }
    if b[*at..].starts_with(b"...") {
        *at += 3;
        return Some(Node::Leaf(Leaf::Rest));
    }
    if b.get(*at) == Some(&b'*') {
        *at += 1;
        return Some(Node::Leaf(Leaf::Next));
    }
    let start = *at;
    while b
        .get(*at)
        .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'-')
    {
        *at += 1;
    }
    (*at > start).then(|| {
        Node::Leaf(Leaf::App(
            String::from_utf8_lossy(&b[start..*at]).to_ascii_lowercase(),
        ))
    })
}

/// A rectangle: x, y, width, height.
pub type Rect = (usize, usize, usize, usize);

/// Each leaf's rectangle in `area`, in leaf order, `gap` pixels between
/// neighbours. The last part of a split takes what rounding leaves.
pub fn rects(node: &Node, area: Rect, gap: usize) -> Vec<Rect> {
    let mut out = Vec::new();
    place(node, area, gap, &mut out);
    out
}

fn place(node: &Node, (x, y, w, h): Rect, gap: usize, out: &mut Vec<Rect>) {
    let Node::Split { across, parts } = node else {
        out.push((x, y, w, h));
        return;
    };
    let total: u32 = parts.iter().map(|(_, w)| w).sum();
    let len = if *across { w } else { h };
    let room = len.saturating_sub(gap * (parts.len() - 1));
    let mut pos = 0;
    for (k, (child, weight)) in parts.iter().enumerate() {
        let size = if k + 1 == parts.len() {
            len - pos
        } else {
            room * *weight as usize / total as usize
        };
        let r = if *across {
            (x + pos, y, size, h)
        } else {
            (x, y + pos, w, size)
        };
        place(child, r, gap, out);
        pos += size + gap;
    }
}

/// A tree of `n` leaves: `n` slots side by side.
fn row(across: bool, leaves: Vec<Node>) -> Node {
    if leaves.len() == 1 {
        return leaves.into_iter().next().unwrap_or(Node::Leaf(Leaf::Next));
    }
    Node::Split {
        across,
        parts: leaves.into_iter().map(|n| (n, 1)).collect(),
    }
}

/// The layout a request's words describe, with `apps` (the apps it names,
/// in order) in its first leaves and `*` in the rest; `None` when the words
/// describe no layout beyond plain columns (the split handles those).
///
/// - "rows", "stacked", "on top of each other", "vertically": `v(...)`
/// - "quarters", "quadrants", "corners", "grid", "two by two": `v(h(a,b),h(c,d))`
/// - "N on top and M below/on the bottom": `v(h(N),h(M))`
/// - "one on the left and N on the right" with "stacked", or "big on the
///   left": `h(a:2,v(...))`
pub fn from_words(request: &str, apps: &[&str]) -> Option<Node> {
    let words: Vec<String> = request
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_ascii_lowercase())
        .collect();
    let has = |w: &str| words.iter().any(|x| x == w);
    let phrase = |p: &str| {
        let p: Vec<&str> = p.split(' ').collect();
        words
            .windows(p.len())
            .any(|win| win.iter().zip(&p).all(|(a, b)| a == b))
    };
    let number = |w: &str| -> Option<usize> {
        ["one", "two", "three", "four"]
            .iter()
            .position(|n| *n == w)
            .map(|k| k + 1)
            .or_else(|| w.parse().ok().filter(|n| (1..=4).contains(n)))
    };
    // Leaves: named apps first, then `*`.
    let mut named = apps.iter().map(|a| a.to_ascii_lowercase());
    let mut slot = || {
        named
            .next()
            .map_or(Node::Leaf(Leaf::Next), |a| Node::Leaf(Leaf::App(a)))
    };

    if has("quarters")
        || has("quadrants")
        || has("corners")
        || has("grid")
        || phrase("two by two")
        || phrase("2 by 2")
    {
        let top = row(true, alloc::vec![slot(), slot()]);
        let bottom = row(true, alloc::vec![slot(), slot()]);
        return Some(row(false, alloc::vec![top, bottom]));
    }
    // "two on top and one below" / "... on the bottom".
    let on_top = words.windows(3).find_map(|w| {
        (w[1] == "on" && w[2] == "top")
            .then(|| number(&w[0]))
            .flatten()
    });
    let below = words
        .windows(4)
        .find_map(|w| {
            ((w[1] == "on" && w[2] == "the" && w[3] == "bottom") || (w[1] == "below"))
                .then(|| number(&w[0]))
                .flatten()
        })
        .or_else(|| {
            words.windows(2).find_map(|w| {
                (w[1] == "below" || w[1] == "underneath")
                    .then(|| number(&w[0]))
                    .flatten()
            })
        });
    if let (Some(t), Some(b)) = (on_top, below)
        && t + b <= MAX_LEAVES
    {
        let top = row(true, (0..t).map(|_| slot()).collect());
        let bottom = row(true, (0..b).map(|_| slot()).collect());
        return Some(row(false, alloc::vec![top, bottom]));
    }
    // "big on the left" / "one on the left and two stacked on the right".
    let stacked = has("stacked")
        || has("stack")
        || phrase("on top of each other")
        || has("vertically")
        || has("rows");
    let big_left =
        phrase("big on the left") || phrase("bigger on the left") || phrase("large on the left");
    let left_one = phrase("one on the left") || phrase("1 on the left");
    let right_n = words.windows(2).find_map(|w| {
        (w[1] == "on" || w[1] == "stacked")
            .then(|| number(&w[0]))
            .flatten()
    });
    if big_left || (left_one && stacked) {
        let first = slot();
        let n = right_n.filter(|n| *n >= 2 && *n < MAX_LEAVES).unwrap_or(2);
        let right = row(false, (0..n).map(|_| slot()).collect());
        return Some(Node::Split {
            across: true,
            parts: alloc::vec![(first, if big_left { 2 } else { 1 }), (right, 1)],
        });
    }
    if stacked || has("row") {
        // "in rows": the named apps, else every window in front.
        if apps.is_empty() {
            return Some(rows_of_rest());
        }
        let leaves: Vec<Node> = (0..apps.len()).map(|_| slot()).collect();
        return Some(row(false, leaves));
    }
    None
}

/// `v(...)`: every window in front, stacked.
fn rows_of_rest() -> Node {
    Node::Split {
        across: false,
        parts: alloc::vec![(Node::Leaf(Leaf::Rest), 1)],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_prints() {
        for t in [
            "h(files,notes)",
            "v(files,notes,network)",
            "v(h(*,*),*)",
            "h(notes:2,v(files,network))",
            "v(h(*,*),h(*,*))",
        ] {
            assert_eq!(parse(t).unwrap().text(), t);
        }
        assert_eq!(parse("V( Files , notes )"), None, "upper-case split letter");
        assert_eq!(parse("h(files)"), None, "one part");
        assert_eq!(parse("h(a,b"), None);
        assert_eq!(parse("h(a:9,b)"), None, "weight too big");
        assert_eq!(parse("h(a,b,c,d,e,f,g)"), None, "too many windows");
        assert_eq!(parse("h(v(h(v(a,b),c),d),e)"), None, "too deep");
    }

    #[test]
    fn rectangles_tile_the_area() {
        let area = (24, 38, 1872, 948);
        let quarters = parse("v(h(a,b),h(c,d))").unwrap();
        let r = rects(&quarters, area, 12);
        assert_eq!(r.len(), 4);
        assert_eq!(r[0], (24, 38, 930, 468));
        assert_eq!(r[1], (24 + 930 + 12, 38, 1872 - 930 - 12, 468));
        assert_eq!(r[2].1, 38 + 468 + 12);
        assert_eq!(r[3].0 + r[3].2, 24 + 1872);
        assert_eq!(r[3].1 + r[3].3, 38 + 948);
        // Weights: two thirds and one third.
        let big = rects(&parse("h(a:2,b)").unwrap(), area, 12);
        assert_eq!(big[0].2, (1872 - 12) * 2 / 3);
        assert_eq!(big[1].0 + big[1].2, 24 + 1872);
    }

    #[test]
    fn rest_expands() {
        let t = parse("v(...)").unwrap();
        assert_eq!(rows_of_rest(), t);
        assert_eq!(t.expand_rest(3).text(), "v(*,*,*)");
        assert_eq!(
            parse("h(notes,...)").unwrap().expand_rest(2).text(),
            "h(notes,*,*)"
        );
        assert_eq!(parse("v(notes)"), None, "one part that is not `...`");
    }

    #[test]
    fn words_to_layouts() {
        let w = |r: &str, apps: &[&str]| from_words(r, apps).map(|n| n.text());
        assert_eq!(w("split these in rows", &[]).as_deref(), Some("v(...)"));
        assert_eq!(
            w("stack notes and files", &["Notes", "Files"]).as_deref(),
            Some("v(notes,files)")
        );
        assert_eq!(
            w("split the screen into quarters", &[]).as_deref(),
            Some("v(h(*,*),h(*,*))")
        );
        assert_eq!(
            w("split it three ways, two on top and one on the bottom", &[]).as_deref(),
            Some("v(h(*,*),*)")
        );
        assert_eq!(
            w(
                "two on top and one below with files, files and notes",
                &["Files", "Files", "Notes"]
            )
            .as_deref(),
            Some("v(h(files,files),notes)")
        );
        assert_eq!(
            w(
                "notes big on the left and the rest stacked on the right",
                &["Notes"]
            )
            .as_deref(),
            Some("h(notes:2,v(*,*))")
        );
        assert_eq!(
            w(
                "put notes on the left and files on the right",
                &["Notes", "Files"]
            ),
            None
        );
        assert_eq!(w("split these two windows", &[]), None);
    }
}
