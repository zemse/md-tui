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
    let path: PathBuf = match base_dir {
        Some(b) => b.join(path_part),
        None => PathBuf::from(path_part),
    };
    match anchor {
        Some(a) => LinkTarget::FileAnchor(path, slugify(&a)),
        None => LinkTarget::LocalFile(path),
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
