use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use ratatui::layout::Rect;

use crate::links::LinkTarget;
use crate::markdown::{self, Rendered};
use crate::theme::Theme;

#[derive(Clone, Debug)]
pub enum Source {
    File(PathBuf),
    Directory(PathBuf),
    Stdin(String),
}

#[derive(Clone, Debug)]
pub struct Options {
    pub width: u16,
    pub line_numbers: bool,
    pub theme: Theme,
}

pub struct App {
    pub view: View,
    /// Search root — set at launch from the file/dir argument; never moves.
    pub root: PathBuf,
    pub history: Vec<HistoryEntry>,
    pub forward: Vec<HistoryEntry>,
    pub opts: Options,
    pub help_open: bool,
    /// `Some` while the fuzzy search overlay is active.
    pub search: Option<Search>,
    pub should_quit: bool,
    pub status: String,
    pub viewport: Rect,
}

pub enum View {
    Reader(Reader),
    Browser(Browser),
}

#[derive(Clone)]
pub struct HistoryEntry {
    pub kind: EntryKind,
    pub scroll: u16,
}

#[derive(Clone)]
pub enum EntryKind {
    File(PathBuf),
    Directory(PathBuf),
    Stdin(String),
}

pub struct Reader {
    pub origin: ReaderOrigin,
    pub raw: String,
    pub rendered: Option<Rendered>,
    pub scroll: u16,
    pub focused_link: Option<usize>,
    pub hover_link: Option<usize>,
}

pub enum ReaderOrigin {
    File(PathBuf),
    Stdin,
}

pub struct Browser {
    pub dir: PathBuf,
    pub entries: Vec<BrowserEntry>,
    pub selected: usize,
    pub scroll: u16,
}

#[derive(Clone)]
pub struct BrowserEntry {
    pub path: PathBuf,
    pub display: String,
    pub kind: BrowserEntryKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrowserEntryKind {
    ParentDir,
    Dir,
    Markdown,
    Other,
}

pub struct Search {
    pub query: String,
    pub results: Vec<SearchResult>,
    pub selected: usize,
    /// Pre-built index of paths under the search root. Filtered by `query`
    /// each time the query changes.
    paths: Vec<IndexedPath>,
}

#[derive(Clone)]
struct IndexedPath {
    path: PathBuf,
    display: String,
    display_lower: String,
    is_dir: bool,
}

#[derive(Clone)]
pub struct SearchResult {
    pub path: PathBuf,
    pub display: String,
    pub score: i32,
    pub is_dir: bool,
}

impl App {
    pub fn new(source: Source, opts: Options) -> Result<Self> {
        let root = derive_root(&source);
        let view = match source {
            Source::File(p) => View::Reader(Reader::from_file(&p)?),
            Source::Directory(d) => View::Browser(Browser::scan(&d)?),
            Source::Stdin(text) => View::Reader(Reader::from_string(text)),
        };
        Ok(Self {
            view,
            root,
            history: Vec::new(),
            forward: Vec::new(),
            opts,
            help_open: false,
            search: None,
            should_quit: false,
            status: String::new(),
            viewport: Rect::new(0, 0, 0, 0),
        })
    }

    pub fn record_current(&self) -> HistoryEntry {
        match &self.view {
            View::Reader(r) => HistoryEntry {
                kind: match &r.origin {
                    ReaderOrigin::File(p) => EntryKind::File(p.clone()),
                    ReaderOrigin::Stdin => EntryKind::Stdin(r.raw.clone()),
                },
                scroll: r.scroll,
            },
            View::Browser(b) => HistoryEntry {
                kind: EntryKind::Directory(b.dir.clone()),
                scroll: b.scroll,
            },
        }
    }

    pub fn navigate_to(&mut self, kind: EntryKind, scroll: u16) -> Result<()> {
        self.forward.clear();
        let prev = self.record_current();
        self.history.push(prev);
        self.load(kind, scroll)
    }

    pub fn go_back(&mut self) -> Result<()> {
        if let Some(prev) = self.history.pop() {
            let cur = self.record_current();
            self.forward.push(cur);
            self.load(prev.kind, prev.scroll)?;
        }
        Ok(())
    }

    pub fn go_forward(&mut self) -> Result<()> {
        if let Some(next) = self.forward.pop() {
            let cur = self.record_current();
            self.history.push(cur);
            self.load(next.kind, next.scroll)?;
        }
        Ok(())
    }

    fn load(&mut self, kind: EntryKind, scroll: u16) -> Result<()> {
        self.view = match kind {
            EntryKind::File(p) => {
                let mut r = Reader::from_file(&p)?;
                r.scroll = scroll;
                View::Reader(r)
            }
            EntryKind::Directory(d) => {
                let mut b = Browser::scan(&d)?;
                b.scroll = scroll;
                View::Browser(b)
            }
            EntryKind::Stdin(text) => {
                let mut r = Reader::from_string(text);
                r.scroll = scroll;
                View::Reader(r)
            }
        };
        Ok(())
    }

    pub fn open_search(&mut self) {
        self.search = Some(Search::build(&self.root));
    }

    pub fn close_search(&mut self) {
        self.search = None;
    }

    /// Resolve and follow a link target. Returns `Ok(true)` if the action was
    /// handled internally (navigation), `Ok(false)` if it was external.
    pub fn follow(&mut self, target: LinkTarget) -> Result<bool> {
        match target {
            LinkTarget::Url(url) => {
                let _ = open::that_detached(&url);
                self.status = format!("Opened {}", url);
                Ok(false)
            }
            LinkTarget::Anchor(slug) => {
                self.scroll_to_anchor(&slug);
                Ok(true)
            }
            LinkTarget::LocalFile(p) => {
                let resolved = match resolve_local_path(&p) {
                    Some(r) => r,
                    None => {
                        self.status = format!("Not found: {}", p.display());
                        return Ok(false);
                    }
                };
                if resolved.is_dir() {
                    self.navigate_to(EntryKind::Directory(resolved), 0)?;
                    Ok(true)
                } else if is_markdown_file(&resolved) {
                    self.navigate_to(EntryKind::File(resolved), 0)?;
                    Ok(true)
                } else {
                    let _ = open::that_detached(&resolved);
                    self.status = format!("Opened externally: {}", resolved.display());
                    Ok(false)
                }
            }
            LinkTarget::FileAnchor(p, slug) => {
                let resolved = match resolve_local_path(&p) {
                    Some(r) => r,
                    None => {
                        self.status = format!("Not found: {}", p.display());
                        return Ok(false);
                    }
                };
                if !is_markdown_file(&resolved) {
                    let _ = open::that_detached(&resolved);
                    self.status = format!("Opened externally: {}", resolved.display());
                    return Ok(false);
                }
                self.navigate_to(EntryKind::File(resolved), 0)?;
                self.scroll_to_anchor(&slug);
                Ok(true)
            }
        }
    }

    fn scroll_to_anchor(&mut self, slug: &str) {
        if let View::Reader(r) = &mut self.view {
            if let Some(rendered) = &r.rendered {
                if let Some(&line) = rendered.link_map.anchors.get(slug) {
                    r.scroll = line as u16;
                    self.status = format!("→ #{}", slug);
                } else {
                    self.status = format!("Anchor not found: #{}", slug);
                }
            }
        }
    }

    /// Re-render reader if width changed since last render.
    pub fn ensure_rendered(&mut self, width: u16) {
        let theme = self.opts.theme.clone();
        let user_width = self.opts.width;
        let target_w = if user_width == 0 { width } else { user_width.min(width) };
        if let View::Reader(r) = &mut self.view {
            let needs = match &r.rendered {
                Some(rd) => rd.width != target_w,
                None => true,
            };
            if needs {
                let base_dir = match &r.origin {
                    ReaderOrigin::File(p) => p.parent().map(|p| p.to_path_buf()),
                    ReaderOrigin::Stdin => None,
                };
                r.rendered = Some(markdown::render(&r.raw, base_dir.as_deref(), target_w, &theme));
                if let Some(rd) = &r.rendered {
                    let max_scroll = rd.lines.len().saturating_sub(1) as u16;
                    if r.scroll > max_scroll { r.scroll = max_scroll; }
                }
            }
        }
    }
}

fn derive_root(source: &Source) -> PathBuf {
    let base = match source {
        Source::File(p) => p
            .parent()
            .map(|x| x.to_path_buf())
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))),
        Source::Directory(d) => d.clone(),
        Source::Stdin(_) => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };
    std::fs::canonicalize(&base).unwrap_or(base)
}

pub fn is_markdown_file(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| matches!(e.to_ascii_lowercase().as_str(), "md" | "markdown" | "mdown" | "mkd"))
        .unwrap_or(false)
}

/// Resolve a local link's path: try as-is, then with a `.md` extension as a
/// fallback. Returns `None` if neither variant exists.
fn resolve_local_path(p: &Path) -> Option<PathBuf> {
    if p.exists() {
        return Some(canonicalize_or(p.to_path_buf()));
    }
    if p.extension().is_none() {
        let with_md = p.with_extension("md");
        if with_md.exists() {
            return Some(canonicalize_or(with_md));
        }
    }
    None
}

fn canonicalize_or(p: PathBuf) -> PathBuf {
    std::fs::canonicalize(&p).unwrap_or(p)
}

impl Reader {
    pub fn from_file(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| anyhow!("read {}: {}", path.display(), e))?;
        Ok(Self {
            origin: ReaderOrigin::File(path.to_path_buf()),
            raw,
            rendered: None,
            scroll: 0,
            focused_link: None,
            hover_link: None,
        })
    }

    pub fn from_string(raw: String) -> Self {
        Self {
            origin: ReaderOrigin::Stdin,
            raw,
            rendered: None,
            scroll: 0,
            focused_link: None,
            hover_link: None,
        }
    }
}

impl Browser {
    /// One-level directory listing: a `..` entry (if there is a parent),
    /// then sub-directories, then files; each group sorted case-insensitively.
    pub fn scan(dir: &Path) -> Result<Self> {
        let mut entries = Vec::new();
        if let Some(parent) = dir.parent() {
            if parent != dir {
                entries.push(BrowserEntry {
                    path: parent.to_path_buf(),
                    display: "../".to_string(),
                    kind: BrowserEntryKind::ParentDir,
                });
            }
        }
        let mut dirs: Vec<(String, PathBuf)> = Vec::new();
        let mut files: Vec<(String, PathBuf)> = Vec::new();
        let read = std::fs::read_dir(dir)
            .map_err(|e| anyhow!("read_dir {}: {}", dir.display(), e))?;
        for entry in read.flatten() {
            let name = match entry.file_name().into_string() {
                Ok(n) => n,
                Err(_) => continue,
            };
            if name.starts_with('.') { continue; }
            let path = entry.path();
            let ft = match entry.file_type() { Ok(f) => f, Err(_) => continue };
            if ft.is_dir() {
                dirs.push((name, path));
            } else if ft.is_file() {
                files.push((name, path));
            }
        }
        let by_name = |a: &(String, PathBuf), b: &(String, PathBuf)| {
            a.0.to_ascii_lowercase().cmp(&b.0.to_ascii_lowercase())
        };
        dirs.sort_by(by_name);
        files.sort_by(by_name);
        for (name, path) in dirs {
            entries.push(BrowserEntry {
                path,
                display: format!("{}/", name),
                kind: BrowserEntryKind::Dir,
            });
        }
        for (name, path) in files {
            let kind = if is_markdown_file(&path) {
                BrowserEntryKind::Markdown
            } else {
                BrowserEntryKind::Other
            };
            entries.push(BrowserEntry { path, display: name, kind });
        }
        Ok(Self {
            dir: dir.to_path_buf(),
            entries,
            selected: 0,
            scroll: 0,
        })
    }

    #[allow(dead_code)]
    pub fn selected_entry(&self) -> Option<&BrowserEntry> {
        self.entries.get(self.selected)
    }
}

impl Search {
    /// Build the index by walking `root` (depth-capped, hidden dirs skipped).
    pub fn build(root: &Path) -> Self {
        let mut paths = Vec::new();
        for entry in walkdir::WalkDir::new(root)
            .follow_links(false)
            .max_depth(8)
            .into_iter()
            .filter_entry(|e| !is_hidden_walk(e))
            .filter_map(|e| e.ok())
        {
            if entry.path() == root { continue; }
            let display = entry
                .path()
                .strip_prefix(root)
                .unwrap_or(entry.path())
                .display()
                .to_string();
            let display_lower = display.to_ascii_lowercase();
            let is_dir = entry.file_type().is_dir();
            paths.push(IndexedPath {
                path: entry.path().to_path_buf(),
                display,
                display_lower,
                is_dir,
            });
        }
        let mut s = Self {
            query: String::new(),
            results: Vec::new(),
            selected: 0,
            paths,
        };
        s.refresh();
        s
    }

    pub fn refresh(&mut self) {
        self.results.clear();
        let q = self.query.to_ascii_lowercase();
        if q.is_empty() {
            for ip in &self.paths {
                self.results.push(SearchResult {
                    path: ip.path.clone(),
                    display: ip.display.clone(),
                    score: 0,
                    is_dir: ip.is_dir,
                });
            }
            self.results.sort_by(|a, b| {
                b.is_dir
                    .cmp(&a.is_dir)
                    .then(a.display.to_ascii_lowercase().cmp(&b.display.to_ascii_lowercase()))
            });
        } else {
            for ip in &self.paths {
                if let Some(score) = score_substring(&ip.display_lower, &q) {
                    self.results.push(SearchResult {
                        path: ip.path.clone(),
                        display: ip.display.clone(),
                        score,
                        is_dir: ip.is_dir,
                    });
                }
            }
            self.results.sort_by(|a, b| {
                b.score
                    .cmp(&a.score)
                    .then(a.display.to_ascii_lowercase().cmp(&b.display.to_ascii_lowercase()))
            });
        }
        if self.selected >= self.results.len() {
            self.selected = 0;
        }
    }

    pub fn move_selection(&mut self, delta: i32) {
        let n = self.results.len() as i32;
        if n == 0 { return; }
        let new = ((self.selected as i32 + delta) % n + n) % n;
        self.selected = new as usize;
    }
}

fn is_hidden_walk(entry: &walkdir::DirEntry) -> bool {
    entry
        .file_name()
        .to_str()
        .map(|s| s.starts_with('.') && s != "." && s != "..")
        .unwrap_or(false)
}

/// Substring score: higher when the match starts earlier, at a word boundary,
/// or in the basename. Returns `None` if `pattern` is not a substring.
fn score_substring(text: &str, pattern: &str) -> Option<i32> {
    let idx = text.find(pattern)?;
    let mut score = 1000 - idx as i32;
    if idx == 0 {
        score += 500;
    } else if let Some(prev) = text.as_bytes().get(idx - 1) {
        if matches!(*prev as char, '/' | '_' | '-' | '.' | ' ') {
            score += 250;
        }
    }
    if let Some(slash) = text.rfind('/') {
        if idx > slash {
            score += 100;
        }
    } else {
        score += 100;
    }
    score -= text.len() as i32 / 4;
    Some(score)
}
