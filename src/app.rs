use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
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
    pub history: Vec<HistoryEntry>,
    pub forward: Vec<HistoryEntry>,
    pub opts: Options,
    pub help_open: bool,
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
    pub root: PathBuf,
    pub entries: Vec<BrowserEntry>,
    pub selected: usize,
    pub scroll: u16,
}

#[derive(Clone)]
pub struct BrowserEntry {
    pub path: PathBuf,
    pub display: String,
}

impl App {
    pub fn new(source: Source, opts: Options) -> Result<Self> {
        let view = match source {
            Source::File(p) => View::Reader(Reader::from_file(&p)?),
            Source::Directory(d) => View::Browser(Browser::scan(&d)?),
            Source::Stdin(text) => View::Reader(Reader::from_string(text)),
        };
        Ok(Self {
            view,
            history: Vec::new(),
            forward: Vec::new(),
            opts,
            help_open: false,
            should_quit: false,
            status: String::new(),
            viewport: Rect::new(0, 0, 0, 0),
        })
    }

    #[allow(dead_code)]
    pub fn current_path(&self) -> Option<&Path> {
        match &self.view {
            View::Reader(r) => match &r.origin {
                ReaderOrigin::File(p) => Some(p.as_path()),
                ReaderOrigin::Stdin => None,
            },
            View::Browser(b) => Some(b.root.as_path()),
        }
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
                kind: EntryKind::Directory(b.root.clone()),
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
                let path = canonicalize_or(p);
                if !path.exists() {
                    self.status = format!("Not found: {}", path.display());
                    return Ok(false);
                }
                if path.is_dir() {
                    self.navigate_to(EntryKind::Directory(path), 0)?;
                } else {
                    self.navigate_to(EntryKind::File(path), 0)?;
                }
                Ok(true)
            }
            LinkTarget::FileAnchor(p, slug) => {
                let path = canonicalize_or(p);
                if !path.exists() {
                    self.status = format!("Not found: {}", path.display());
                    return Ok(false);
                }
                self.navigate_to(EntryKind::File(path), 0)?;
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
    pub fn scan(root: &Path) -> Result<Self> {
        let mut entries = Vec::new();
        for entry in walkdir::WalkDir::new(root)
            .follow_links(false)
            .max_depth(8)
            .into_iter()
            .filter_entry(|e| !is_hidden(e))
            .filter_map(|e| e.ok())
        {
            if !entry.file_type().is_file() { continue; }
            let p = entry.path();
            let ext = p.extension().and_then(|s| s.to_str()).unwrap_or("");
            if !matches!(ext.to_ascii_lowercase().as_str(), "md" | "markdown" | "mdown" | "mkd") {
                continue;
            }
            let display = p
                .strip_prefix(root)
                .unwrap_or(p)
                .display()
                .to_string();
            entries.push(BrowserEntry { path: p.to_path_buf(), display });
        }
        entries.sort_by(|a, b| a.display.cmp(&b.display));
        Ok(Self {
            root: root.to_path_buf(),
            entries,
            selected: 0,
            scroll: 0,
        })
    }

    pub fn selected_path(&self) -> Option<&Path> {
        self.entries.get(self.selected).map(|e| e.path.as_path())
    }
}

fn is_hidden(entry: &walkdir::DirEntry) -> bool {
    entry
        .file_name()
        .to_str()
        .map(|s| s.starts_with('.') && s != "." && s != "..")
        .unwrap_or(false)
}
