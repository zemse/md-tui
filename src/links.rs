use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkTarget {
    /// External URL — opened in the system browser.
    Url(String),
    /// Local markdown / text file — navigated to within the app.
    LocalFile(PathBuf),
    /// Anchor in the current document.
    Anchor(String),
    /// Anchor in another local file.
    FileAnchor(PathBuf, String),
}

#[derive(Clone, Debug)]
pub struct LinkSpan {
    pub line: usize,
    pub col_start: usize,
    pub col_end: usize,
    pub target: LinkTarget,
}

#[derive(Default, Clone, Debug)]
pub struct LinkMap {
    pub links: Vec<LinkSpan>,
    /// heading anchor id -> line index
    pub anchors: HashMap<String, usize>,
}

/// A `[ ]` / `[x]` task-list marker, located in both rendered output (line/col)
/// and the original source (byte offset of the opening `[`).
#[derive(Clone, Debug)]
pub struct CheckboxSpan {
    pub line: usize,
    pub col_start: usize,
    pub col_end: usize,
    pub source_offset: usize,
    pub checked: bool,
}

#[derive(Default, Clone, Debug)]
pub struct CheckboxMap {
    pub items: Vec<CheckboxSpan>,
}

impl CheckboxMap {
    pub fn at(&self, line: usize, col: usize) -> Option<usize> {
        self.items
            .iter()
            .position(|c| c.line == line && col >= c.col_start && col < c.col_end)
    }
}

#[allow(dead_code)]
impl LinkMap {
    /// Returns the index of the link containing (line, col), if any.
    pub fn at(&self, line: usize, col: usize) -> Option<usize> {
        self.links.iter().position(|l| l.line == line && col >= l.col_start && col < l.col_end)
    }

    /// First link on or after `line`.
    pub fn next_from(&self, line: usize, col: usize) -> Option<usize> {
        self.links
            .iter()
            .enumerate()
            .find(|(_, l)| l.line > line || (l.line == line && l.col_start > col))
            .map(|(i, _)| i)
    }

    pub fn prev_from(&self, line: usize, col: usize) -> Option<usize> {
        self.links
            .iter()
            .enumerate()
            .rev()
            .find(|(_, l)| l.line < line || (l.line == line && l.col_end <= col))
            .map(|(i, _)| i)
    }
}

/// Resolve a markdown link's `dest_url` into a typed [`LinkTarget`].
pub fn resolve(dest: &str, base_dir: Option<&Path>) -> LinkTarget {
    if dest.starts_with('#') {
        return LinkTarget::Anchor(slugify(&dest[1..]));
    }
    if let Some((scheme, _)) = dest.split_once("://") {
        if !scheme.is_empty() { return LinkTarget::Url(dest.to_string()); }
    }
    if dest.starts_with("mailto:") || dest.starts_with("tel:") {
        return LinkTarget::Url(dest.to_string());
    }
    let (path_part, anchor) = match dest.split_once('#') {
        Some((p, a)) => (p, Some(a.to_string())),
        None => (dest, None),
    };
    let decoded = percent_decode(path_part);
    let path: PathBuf = match base_dir {
        Some(b) => b.join(&decoded),
        None => PathBuf::from(decoded),
    };
    match anchor {
        Some(a) => LinkTarget::FileAnchor(path, slugify(&a)),
        None => LinkTarget::LocalFile(path),
    }
}

/// Minimal URL percent-decoder for local link paths (handles `%20` and friends).
/// UTF-8 aware: bytes are decoded then re-validated; falls back to the original
/// string if the result isn't valid UTF-8.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push(hi * 16 + lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Convert heading text to a GitHub-style anchor slug.
pub fn slugify(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_dash = false;
    for ch in s.chars() {
        if ch.is_alphanumeric() {
            for c in ch.to_lowercase() { out.push(c); }
            prev_dash = false;
        } else if ch.is_whitespace() || ch == '-' || ch == '_' {
            if !prev_dash && !out.is_empty() {
                out.push('-');
                prev_dash = true;
            }
        }
    }
    while out.ends_with('-') { out.pop(); }
    out
}
