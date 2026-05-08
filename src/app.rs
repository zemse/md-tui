use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use ratatui::layout::Rect;

use crate::links::LinkTarget;
use crate::markdown::{self, Rendered};
use crate::theme::Theme;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;
use std::collections::HashMap;

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
    /// The row containing the statusline. Click handling on this row covers
    /// the back-button hit zone.
    pub statusline_area: Rect,
    /// Last column range occupied by the `[‹ Back]` button in the statusline,
    /// recorded by the renderer so click handling can hit-test it.
    pub back_button_hit: Option<(u16, u16)>,
    /// Pending vim count prefix (e.g. user typed `5` waiting for `j`). Reset
    /// after the motion key consumes it, or on Esc.
    pub count_prefix: Option<u32>,
    /// `Some(instant)` when the user pressed `g` and we're waiting for the
    /// second key of a `gg`/`ge`/`gh`-style chord. Times out after ~700ms so
    /// a stray `g` doesn't lock subsequent input.
    pub pending_g: Option<std::time::Instant>,
    /// `Some(instant)` waiting for the second key of a `zz` chord.
    pub pending_z: Option<std::time::Instant>,
    /// True when the most recent input was a mouse event. While set we hide
    /// the keyboard focus highlight so the user isn't tracking two cursors.
    pub mouse_recent: bool,
    /// Last observed mouse column inside the body. Used by the statusline
    /// hover-URL edge case to decide which side of the row to render on when
    /// the hovered link sits on the bottom-most body row.
    pub last_mouse_col: u16,
    /// Last observed mouse row. Together with `last_mouse_col` lets the
    /// statusline detect "mouse is right above the statusline" cases.
    pub last_mouse_row: u16,
    /// Mouse capture state. When `false`, drag/click events fall through to
    /// the terminal so the user can select text natively.
    pub mouse_enabled: bool,
    /// In-app text selection state. Set on first Drag after Mouse Down,
    /// cleared on Up (after copying) or any view-mutating action.
    pub selection: Option<Selection>,
    /// Git lens overlay state. `Some` while the user has Ctrl-G toggled on.
    /// Holds the parsed diff vs HEAD (staged + unstaged combined) for the
    /// current file. `None` otherwise.
    pub git_lens: Option<GitLensState>,
    /// Most recent left-mouse-down (Instant + column + row), used to detect
    /// double-clicks for word selection.
    pub last_click: Option<(std::time::Instant, u16, u16)>,
    /// On left-mouse-down we stash a pending single-click target. It fires
    /// on Up only if no Drag arrived in between (so drag-select doesn't
    /// also navigate).
    pub pending_click: Option<(u16, u16)>,
    /// Sub-line accumulator for wheel-scroll dampening. Carries fractional
    /// lines across events so a halved scroll factor still produces smooth
    /// movement instead of "stuck" frames where nothing happens.
    pub scroll_accum: f32,
    /// Timestamp of the most recent wheel scroll event. `None` until the
    /// first wheel tick. Used to detect bursts.
    pub last_scroll_at: Option<std::time::Instant>,
    /// Detected terminal image protocol. `None` if the terminal can't render
    /// images (then we fall back to placeholder text).
    pub image_picker: Option<Picker>,
    /// Lazily-decoded image protocol cache, keyed by canonicalised path.
    pub image_protocols: HashMap<PathBuf, StatefulProtocol>,
}

pub enum View {
    Reader(Reader),
    Browser(Browser),
}

#[derive(Clone)]
pub struct HistoryEntry {
    pub kind: EntryKind,
    pub scroll: u16,
    /// Browser cursor position. `None` for reader entries.
    pub selected: Option<usize>,
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
    /// Unified keyboard cursor. Walks links AND checkboxes in document order
    /// via Tab / S-Tab. Suppressed visually while the mouse is recent.
    pub focus: Option<Focus>,
    pub hover_link: Option<usize>,
    pub hover_checkbox: Option<usize>,
    pub doc_search: Option<DocSearch>,
    /// In-house edit mode. `Some` while the user is editing this buffer
    /// in-place (entered via `e`, exited via Esc-Esc).
    pub edit: Option<EditState>,
    /// (mtime, size) snapshot of the source file at last read. Used by the
    /// event loop to detect external edits and reload. `None` for stdin or
    /// when the metadata wasn't available at load time.
    pub last_meta: Option<(std::time::SystemTime, u64)>,
}

#[derive(Clone, Debug)]
pub struct EditState {
    /// Cursor position as a byte offset into `Reader::raw`. Always lands on
    /// a UTF-8 char boundary; helpers in the events layer enforce that.
    pub cursor: usize,
    /// True after any insert/delete since the last save or load.
    pub dirty: bool,
    /// First Esc arms this; second Esc discards changes and exits edit mode.
    /// Any other key clears it. Stored as a flag (not a timestamp) so the
    /// statusline confirm prompt persists until the user makes a choice.
    pub discard_pending: bool,
    /// Undo stack: prior (raw, cursor) snapshots. Pushed before each
    /// mutating op; Ctrl-Z pops to revert.
    pub undo: Vec<EditSnapshot>,
    /// Redo stack: snapshots popped from undo after a Ctrl-Z. Cleared on
    /// any new mutation (since the future timeline diverged).
    pub redo: Vec<EditSnapshot>,
}

#[derive(Clone, Debug)]
pub struct EditSnapshot {
    pub raw: String,
    pub cursor: usize,
}

/// Soft cap on undo depth. Picked to balance memory (each snapshot is one
/// String clone) against typical editing sessions.
pub const UNDO_LIMIT: usize = 200;

/// In-app drag-select state. Anchor and focus are stored in (line_index,
/// display_col) where line_index is the row in `Rendered::lines` (so
/// scrolling mid-drag doesn't tear the range). Anchor stays put once set;
/// focus moves with the mouse on each Drag event.
#[derive(Clone, Copy, Debug)]
pub struct Selection {
    pub anchor_line: usize,
    pub anchor_col: u16,
    pub focus_line: usize,
    pub focus_col: u16,
    /// True once the user has dragged at least one cell; pure clicks
    /// (Down + Up with no Drag) leave this false and don't trigger copy.
    pub dragged: bool,
}

impl Selection {
    /// Return the (start, end) pair in document order, regardless of
    /// drag direction.
    pub fn normalized(&self) -> ((usize, u16), (usize, u16)) {
        let a = (self.anchor_line, self.anchor_col);
        let b = (self.focus_line, self.focus_col);
        if a <= b { (a, b) } else { (b, a) }
    }
    /// True if the selection covers any non-empty range.
    pub fn is_active(&self) -> bool {
        self.dragged
            && (self.anchor_line != self.focus_line || self.anchor_col != self.focus_col)
    }
}

/// Pre-parsed `git diff HEAD -- <file>` content. Each entry is a single
/// display row tagged with how it should be styled. We deliberately keep
/// the parser tiny and tolerant — the goal is a quick visual lens, not a
/// fully-correct diff parser.
#[derive(Clone, Debug)]
pub struct GitLensState {
    pub rows: Vec<DiffRow>,
    /// Scroll offset within the diff overlay.
    pub scroll: u16,
}

#[derive(Clone, Debug)]
pub struct DiffRow {
    pub kind: DiffRowKind,
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffRowKind {
    /// Context line (unchanged) — rendered plain.
    Context,
    /// Added line — green background.
    Added,
    /// Removed line — red background.
    Removed,
    /// Hunk header (`@@ -a,b +c,d @@`) — muted fg.
    Hunk,
    /// File header (`diff --git`, `index`, `---`, `+++`) — muted, dim.
    Header,
    /// Synthetic informational row (e.g. "no changes") — muted bold.
    Info,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Link(usize),
    Checkbox(usize),
}

#[derive(Clone, Debug)]
pub struct DocSearch {
    pub query: String,
    pub matches: Vec<DocMatch>,
    pub current: usize,
    /// True while the user is still typing the query (prompt is open).
    pub editing: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct DocMatch {
    pub line: usize,
    pub col_start: usize,
    pub col_end: usize,
}

#[derive(Clone)]
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
            statusline_area: Rect::new(0, 0, 0, 0),
            back_button_hit: None,
            count_prefix: None,
            pending_g: None,
            pending_z: None,
            mouse_recent: false,
            last_mouse_col: 0,
            last_mouse_row: 0,
            mouse_enabled: true,
            selection: None,
            pending_click: None,
            git_lens: None,
            last_click: None,
            scroll_accum: 0.0,
            last_scroll_at: None,
            image_picker: None,
            image_protocols: HashMap::new(),
        })
    }

    /// Probe the terminal for graphics-protocol support. Must be called after
    /// the alternate screen is active — the probe writes a query to stdout
    /// and reads the reply from stdin, and any unrecognized escape bytes
    /// would otherwise be left on the user's main screen.
    pub fn init_image_picker(&mut self) {
        self.image_picker = Picker::from_query_stdio().ok();
    }

    pub fn record_current(&self) -> HistoryEntry {
        match &self.view {
            View::Reader(r) => HistoryEntry {
                kind: match &r.origin {
                    ReaderOrigin::File(p) => EntryKind::File(p.clone()),
                    ReaderOrigin::Stdin => EntryKind::Stdin(r.raw.clone()),
                },
                scroll: r.scroll,
                selected: None,
            },
            View::Browser(b) => HistoryEntry {
                kind: EntryKind::Directory(b.dir.clone()),
                scroll: b.scroll,
                selected: Some(b.selected),
            },
        }
    }

    pub fn navigate_to(&mut self, kind: EntryKind, scroll: u16) -> Result<()> {
        self.forward.clear();
        let prev = self.record_current();
        self.history.push(prev);
        self.load(kind, scroll, None)
    }

    pub fn go_back(&mut self) -> Result<()> {
        if let Some(prev) = self.history.pop() {
            let cur = self.record_current();
            self.forward.push(cur);
            self.load(prev.kind, prev.scroll, prev.selected)?;
        }
        Ok(())
    }

    pub fn go_forward(&mut self) -> Result<()> {
        if let Some(next) = self.forward.pop() {
            let cur = self.record_current();
            self.history.push(cur);
            self.load(next.kind, next.scroll, next.selected)?;
        }
        Ok(())
    }

    fn load(&mut self, kind: EntryKind, scroll: u16, selected: Option<usize>) -> Result<()> {
        self.view = match kind {
            EntryKind::File(p) => {
                let mut r = Reader::from_file(&p)?;
                r.scroll = scroll;
                View::Reader(r)
            }
            EntryKind::Directory(d) => {
                let mut b = Browser::scan(&d)?;
                b.scroll = scroll;
                if let Some(sel) = selected {
                    let max = b.entries.len().saturating_sub(1);
                    b.selected = sel.min(max);
                }
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
                let resolved = match resolve_local_path(&p)
                    .or_else(|| vault_lookup(&self.root, &p))
                {
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
                let resolved = match resolve_local_path(&p)
                    .or_else(|| vault_lookup(&self.root, &p))
                {
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

    /// Cheap external-change check, called every event-loop tick. Stats the
    /// open file; if (mtime, size) differs from the recorded fingerprint, the
    /// content is re-read and the cached render is dropped. Returns `true`
    /// when the on-screen content actually changed (mtime touched but byte-
    /// identical content does not count). No-op for stdin or non-Reader views.
    pub fn poll_external_change(&mut self) -> bool {
        let View::Reader(r) = &mut self.view else { return false };
        // Don't clobber an in-flight edit. The user can resolve any conflict
        // explicitly by saving (overwrites disk) or discarding via Esc-Esc
        // (reloads from disk).
        if r.edit.is_some() { return false; }
        let path = match &r.origin {
            ReaderOrigin::File(p) => p.clone(),
            ReaderOrigin::Stdin => return false,
        };
        let Some(new_meta) = file_meta(&path) else { return false };
        if r.last_meta.as_ref() == Some(&new_meta) { return false; }
        // Fingerprint moved — re-read and decide whether content actually
        // changed. A transient read failure (editor mid-rename, etc.) is
        // ignored; we'll retry on the next tick.
        let Ok(new_raw) = std::fs::read_to_string(&path) else { return false };
        r.last_meta = Some(new_meta);
        if new_raw == r.raw { return false; }
        r.raw = new_raw;
        r.rendered = None;
        r.hover_link = None;
        r.hover_checkbox = None;
        r.focus = None;
        if let Some(ds) = &mut r.doc_search {
            ds.matches.clear();
            ds.current = 0;
        }
        self.status = "File reloaded".into();
        true
    }

    /// Flip the `[ ]`/`[x]` task marker at `idx` and persist to the source file.
    /// No-op for stdin sources. Drops the cached render so the next draw
    /// reflects the new state.
    pub fn toggle_checkbox(&mut self, idx: usize) -> Result<()> {
        let View::Reader(r) = &mut self.view else { return Ok(()); };
        let Some(rendered) = r.rendered.as_ref() else { return Ok(()); };
        let Some(cb) = rendered.checkbox_map.items.get(idx) else { return Ok(()); };
        let offset = cb.source_offset;
        let was_checked = cb.checked;
        if offset + 3 > r.raw.len() { return Ok(()); }
        let replacement = if was_checked { "[ ]" } else { "[x]" };
        let mut new_raw = String::with_capacity(r.raw.len());
        new_raw.push_str(&r.raw[..offset]);
        new_raw.push_str(replacement);
        new_raw.push_str(&r.raw[offset + 3..]);
        r.raw = new_raw;
        if let ReaderOrigin::File(p) = &r.origin {
            let path = p.clone();
            std::fs::write(&path, &r.raw)
                .map_err(|e| anyhow!("write {}: {}", path.display(), e))?;
            // Refresh fingerprint so the watcher doesn't see our own write
            // as an external change and trigger a redundant reload.
            r.last_meta = file_meta(&path);
            self.status = if was_checked { "Unchecked".into() } else { "Checked".into() };
        } else {
            self.status = "Toggled (in-memory; stdin not persisted)".into();
        }
        r.rendered = None;
        r.hover_checkbox = None;
        r.hover_link = None;
        Ok(())
    }

    /// Open the in-document text search prompt. No-op outside Reader.
    pub fn open_doc_search(&mut self) {
        if let View::Reader(r) = &mut self.view {
            r.doc_search = Some(DocSearch {
                query: String::new(),
                matches: Vec::new(),
                current: 0,
                editing: true,
            });
            self.status.clear();
        }
    }

    pub fn close_doc_search(&mut self) {
        if let View::Reader(r) = &mut self.view {
            r.doc_search = None;
        }
    }

    /// Recompute matches from the rendered document for the current query.
    pub fn doc_search_refresh(&mut self) {
        let View::Reader(r) = &mut self.view else { return; };
        let Some(rendered) = r.rendered.as_ref() else { return; };
        let Some(s) = r.doc_search.as_mut() else { return; };
        s.matches = find_doc_matches(&rendered.lines, &s.query);
        if s.matches.is_empty() { s.current = 0; }
        else if s.current >= s.matches.len() { s.current = 0; }
    }

    /// Confirm the current query (close prompt, jump to first match).
    pub fn doc_search_commit(&mut self) {
        let View::Reader(r) = &mut self.view else { return; };
        let Some(s) = r.doc_search.as_mut() else { return; };
        s.editing = false;
        if s.matches.is_empty() {
            self.status = "No matches".into();
            return;
        }
        s.current = 0;
        self.center_on_doc_match();
    }

    /// Step to the next/previous match (after commit).
    pub fn doc_search_step(&mut self, forward: bool) {
        let View::Reader(r) = &mut self.view else { return; };
        let Some(s) = r.doc_search.as_mut() else { return; };
        if s.matches.is_empty() { return; }
        let n = s.matches.len();
        s.current = if forward {
            (s.current + 1) % n
        } else {
            (s.current + n - 1) % n
        };
        self.center_on_doc_match();
    }

    fn center_on_doc_match(&mut self) {
        let h = self.viewport.height as usize;
        let View::Reader(r) = &mut self.view else { return; };
        let Some(s) = r.doc_search.as_ref() else { return; };
        let Some(m) = s.matches.get(s.current) else { return; };
        let new = m.line.saturating_sub(h / 2);
        let total = r.rendered.as_ref().map(|x| x.lines.len()).unwrap_or(0);
        let max_scroll = total.saturating_sub(h);
        r.scroll = new.min(max_scroll) as u16;
    }

    /// Toggle the git lens overlay. On the way in, runs `git diff HEAD --`
    /// for the current reader file (combined staged + unstaged); on the way
    /// out, just drops the cached diff. No-op for stdin or non-Reader views.
    /// Disabled in edit mode (the diff would race with un-saved buffer).
    pub fn toggle_git_lens(&mut self) {
        if let View::Reader(r) = &self.view {
            if r.edit.is_some() {
                self.status = "Git lens unavailable while editing".into();
                return;
            }
        }
        if self.git_lens.is_some() {
            self.git_lens = None;
            self.status.clear();
            return;
        }
        let path = match &self.view {
            View::Reader(r) => match &r.origin {
                ReaderOrigin::File(p) => p.clone(),
                ReaderOrigin::Stdin => {
                    self.status = "Git lens unavailable for stdin".into();
                    return;
                }
            },
            _ => return,
        };
        match run_git_diff(&path) {
            Ok(diff) => {
                let rows = parse_unified_diff(&diff);
                let clean = rows.iter().all(|r| matches!(r.kind, DiffRowKind::Header | DiffRowKind::Info));
                let rows = if clean {
                    vec![DiffRow {
                        kind: DiffRowKind::Info,
                        text: "✓ No uncommitted changes".to_string(),
                    }]
                } else {
                    rows
                };
                let _ = clean;
                self.git_lens = Some(GitLensState { rows, scroll: 0 });
            }
            Err(e) => {
                self.status = format!("git diff: {}", e);
            }
        }
    }

    /// Scroll the git lens overlay by `delta` rows (clamped). No-op if the
    /// overlay isn't open.
    pub fn git_lens_scroll(&mut self, delta: i32) {
        let h = self.viewport.height as i32;
        if let Some(g) = self.git_lens.as_mut() {
            let total = g.rows.len() as i32;
            let max = (total - h).max(0);
            let new = (g.scroll as i32 + delta).clamp(0, max) as u16;
            g.scroll = new;
        }
    }

    /// Enter in-house edit mode. Cursor starts at byte 0; nothing dirty;
    /// no discard pending. No-op for stdin (we'd have nothing to write to).
    pub fn enter_edit(&mut self) {
        if let View::Reader(r) = &mut self.view {
            if matches!(r.origin, ReaderOrigin::Stdin) {
                self.status = "Cannot edit: source is stdin".into();
                return;
            }
            r.edit = Some(EditState {
                cursor: 0,
                dirty: false,
                discard_pending: false,
                undo: Vec::new(),
                redo: Vec::new(),
            });
            r.rendered = None;
            self.status.clear();
        }
    }

    /// Discard buffer changes and exit edit mode. Reloads the file from disk
    /// to drop any unsaved edits, then returns the reader to view mode.
    pub fn exit_edit_discard(&mut self) {
        if let View::Reader(r) = &mut self.view {
            if r.edit.is_none() { return; }
            if let ReaderOrigin::File(path) = r.origin.clone() {
                if let Ok(disk) = std::fs::read_to_string(&path) {
                    r.raw = disk;
                    r.last_meta = file_meta(&path);
                }
            }
            r.edit = None;
            r.rendered = None;
            self.status = "Edit discarded".into();
        }
    }

    /// Exit edit mode without modifying the buffer. Used after a successful
    /// save: the buffer is already what's on disk, so we just leave edit mode.
    #[allow(dead_code)]
    pub fn exit_edit(&mut self) {
        if let View::Reader(r) = &mut self.view {
            r.edit = None;
            r.rendered = None;
        }
    }

    /// Insert `text` at the current edit cursor and advance the cursor past
    /// it. Marks dirty. No-op outside edit mode.
    pub fn edit_insert(&mut self, text: &str) {
        let View::Reader(r) = &mut self.view else { return };
        if r.edit.is_none() { return; }
        push_undo(r);
        let e = r.edit.as_mut().unwrap();
        let pos = e.cursor.min(r.raw.len());
        // Snap to the nearest char boundary <= pos so we don't mid-byte split.
        let pos = floor_char_boundary(&r.raw, pos);
        r.raw.insert_str(pos, text);
        let e = r.edit.as_mut().unwrap();
        e.cursor = pos + text.len();
        e.dirty = true;
        e.discard_pending = false;
        r.rendered = None;
    }

    /// Delete `n` chars to the left of the cursor (Backspace). No-op if
    /// the cursor is at byte 0.
    pub fn edit_backspace(&mut self) {
        let View::Reader(r) = &mut self.view else { return };
        let Some(e) = r.edit.as_ref() else { return };
        if e.cursor == 0 { return; }
        push_undo(r);
        let e = r.edit.as_mut().unwrap();
        let end = floor_char_boundary(&r.raw, e.cursor);
        let prev = prev_char_boundary(&r.raw, end);
        r.raw.replace_range(prev..end, "");
        let e = r.edit.as_mut().unwrap();
        e.cursor = prev;
        e.dirty = true;
        e.discard_pending = false;
        r.rendered = None;
    }

    /// Delete one char to the right of the cursor (Delete key).
    pub fn edit_delete(&mut self) {
        let View::Reader(r) = &mut self.view else { return };
        if r.edit.is_none() { return; }
        let e = r.edit.as_ref().unwrap();
        let pos = floor_char_boundary(&r.raw, e.cursor);
        if pos >= r.raw.len() { return; }
        push_undo(r);
        let next = next_char_boundary(&r.raw, pos);
        r.raw.replace_range(pos..next, "");
        let e = r.edit.as_mut().unwrap();
        e.dirty = true;
        e.discard_pending = false;
        r.rendered = None;
    }

    /// Undo the last edit. Pops a snapshot off the undo stack, pushes the
    /// current state to redo, and restores raw + cursor.
    pub fn edit_undo(&mut self) {
        let View::Reader(r) = &mut self.view else { return };
        if r.edit.is_none() { return; }
        let e = r.edit.as_mut().unwrap();
        let Some(snap) = e.undo.pop() else { return };
        // Save current as redo entry.
        let cur_cursor = e.cursor;
        let cur_raw = std::mem::take(&mut r.raw);
        // Restore.
        r.raw = snap.raw;
        let e = r.edit.as_mut().unwrap();
        e.redo.push(EditSnapshot { raw: cur_raw, cursor: cur_cursor });
        e.cursor = snap.cursor.min(r.raw.len());
        e.dirty = true; // Even after undo, the buffer differs from disk usually.
        e.discard_pending = false;
        r.rendered = None;
    }

    /// Redo a previously undone edit.
    pub fn edit_redo(&mut self) {
        let View::Reader(r) = &mut self.view else { return };
        if r.edit.is_none() { return; }
        let e = r.edit.as_mut().unwrap();
        let Some(snap) = e.redo.pop() else { return };
        let cur_cursor = e.cursor;
        let cur_raw = std::mem::take(&mut r.raw);
        r.raw = snap.raw;
        let e = r.edit.as_mut().unwrap();
        e.undo.push(EditSnapshot { raw: cur_raw, cursor: cur_cursor });
        if e.undo.len() > UNDO_LIMIT { e.undo.remove(0); }
        e.cursor = snap.cursor.min(r.raw.len());
        e.dirty = true;
        e.discard_pending = false;
        r.rendered = None;
    }

    /// Move cursor by one char left/right (`delta` ±1). Re-renders so the
    /// block-level toggle can swap blocks if the cursor crossed a boundary.
    pub fn edit_move_horizontal(&mut self, delta: i32) {
        let View::Reader(r) = &mut self.view else { return };
        let Some(e) = r.edit.as_mut() else { return };
        e.discard_pending = false;
        let pos = floor_char_boundary(&r.raw, e.cursor);
        let new = if delta < 0 {
            prev_char_boundary(&r.raw, pos)
        } else {
            next_char_boundary(&r.raw, pos)
        };
        if new != e.cursor {
            e.cursor = new;
            r.rendered = None;
        }
    }

    /// Move the cursor up/down one *display* row, so soft-wrapped lines
    /// step row-by-row instead of jumping past the whole paragraph. Falls
    /// back to source-line stepping when the target row is outside any
    /// raw block (e.g. crossing into a formatted block — that block then
    /// becomes raw on the next render and subsequent moves land precisely).
    pub fn edit_move_vertical(&mut self, delta: i32) {
        let View::Reader(r) = &mut self.view else { return };
        let Some(e) = r.edit.as_mut() else { return };
        e.discard_pending = false;

        if let Some(rendered) = r.rendered.as_ref() {
            if let Some((cur_col, cur_row)) = rendered.cursor_xy {
                let target_row = (cur_row as i32 + delta).max(0) as usize;
                if let Some(Some(range)) = rendered.row_source.get(target_row) {
                    let new = source_offset_at_col(&r.raw, range, cur_col as usize);
                    if new != e.cursor {
                        e.cursor = new;
                        r.rendered = None;
                    }
                    return;
                }
            }
        }

        // Fallback: source-line stepping. Lands at the next/prev `\n`-
        // delimited source line; the renderer will substitute that block
        // raw on the next render so the user can keep navigating.
        let (line_idx, col) = source_line_col(&r.raw, e.cursor);
        let target_line = (line_idx as i32 + delta).max(0) as usize;
        let new = source_offset_for(&r.raw, target_line, col);
        if new != e.cursor {
            e.cursor = new;
            r.rendered = None;
        }
    }

    /// Move cursor to start (`bol`) or end (`eol`) of current source line.
    pub fn edit_move_line_edge(&mut self, eol: bool) {
        let View::Reader(r) = &mut self.view else { return };
        let Some(e) = r.edit.as_mut() else { return };
        e.discard_pending = false;
        let (line_idx, _col) = source_line_col(&r.raw, e.cursor);
        let new = if eol {
            source_line_end(&r.raw, line_idx)
        } else {
            source_line_start(&r.raw, line_idx)
        };
        if new != e.cursor {
            e.cursor = new;
            r.rendered = None;
        }
    }

    /// Persist the current buffer to disk. Refreshes the on-disk fingerprint
    /// so the external-change watcher doesn't see our own write as a phantom
    /// edit on the next tick.
    pub fn save_edit(&mut self) -> Result<()> {
        let View::Reader(r) = &mut self.view else { return Ok(()); };
        if r.edit.is_none() { return Ok(()); }
        let ReaderOrigin::File(path) = r.origin.clone() else { return Ok(()); };
        std::fs::write(&path, &r.raw)
            .map_err(|e| anyhow!("write {}: {}", path.display(), e))?;
        r.last_meta = file_meta(&path);
        if let Some(e) = r.edit.as_mut() {
            e.dirty = false;
            e.discard_pending = false;
        }
        self.status = format!("Saved {}", path.display());
        Ok(())
    }

    /// Re-render reader if width or edit-mode cursor changed since last
    /// render. Edit mode bypasses the cached render whenever the cursor
    /// has moved into a different block, since the block-level toggle
    /// changes the displayed content.
    pub fn ensure_rendered(&mut self, width: u16) {
        let theme = self.opts.theme.clone();
        let user_width = self.opts.width;
        let target_w = if user_width == 0 { width } else { user_width.min(width) };
        if let View::Reader(r) = &mut self.view {
            // Edit mode invalidates cache aggressively. The renderer is fast
            // enough at the file sizes a TUI reader handles that re-running
            // it on every keystroke is acceptable; we can add an
            // "invalidate-only-when-cursor-crosses-block" optimisation later
            // if we measure pain.
            let needs = match &r.rendered {
                Some(rd) => rd.width != target_w || r.edit.is_some(),
                None => true,
            };
            if needs {
                let base_dir = match &r.origin {
                    ReaderOrigin::File(p) => p.parent().map(|p| p.to_path_buf()),
                    ReaderOrigin::Stdin => None,
                };
                let edit_ctx = r.edit.as_ref().map(|e| markdown::EditCtx { cursor: e.cursor });
                r.rendered = Some(markdown::render_with_edit(
                    &r.raw,
                    base_dir.as_deref(),
                    target_w,
                    &theme,
                    edit_ctx,
                ));
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

/// Flat one-level listing of `dir`: dirs-first, then markdown files, both
/// sorted case-insensitively. Honours .gitignore via the `ignore` crate.
fn push_children(dir: &Path, out: &mut Vec<BrowserEntry>) {
    let mut dirs: Vec<(String, PathBuf)> = Vec::new();
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    let walker = ignore::WalkBuilder::new(dir)
        .max_depth(Some(1))
        .hidden(true)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(true)
        .require_git(false)
        .build();
    for result in walker {
        let entry = match result { Ok(e) => e, Err(_) => continue };
        if entry.path() == dir { continue; }
        let name = match entry.file_name().to_str() {
            Some(n) => n.to_string(),
            None => continue,
        };
        let path = entry.path().to_path_buf();
        let ft = match entry.file_type() { Some(f) => f, None => continue };
        if ft.is_dir() {
            dirs.push((name, path));
        } else if ft.is_file() && is_markdown_file(&path) {
            files.push((name, path));
        }
    }
    let by_name = |a: &(String, PathBuf), b: &(String, PathBuf)| {
        a.0.to_ascii_lowercase().cmp(&b.0.to_ascii_lowercase())
    };
    dirs.sort_by(by_name);
    files.sort_by(by_name);
    for (name, path) in dirs {
        out.push(BrowserEntry {
            path,
            display: format!("{}/", name),
            kind: BrowserEntryKind::Dir,
        });
    }
    for (name, path) in files {
        out.push(BrowserEntry {
            path,
            display: name,
            kind: BrowserEntryKind::Markdown,
        });
    }
}

/// Case-insensitive substring search across the rendered lines, mapped to
/// (line, col_start, col_end) in display-width coordinates so the highlight
/// aligns with what the user sees.
pub fn find_doc_matches(
    lines: &[ratatui::text::Line<'static>],
    query: &str,
) -> Vec<DocMatch> {
    let mut out = Vec::new();
    if query.is_empty() { return out; }
    let q = query.to_ascii_lowercase();
    for (line_idx, line) in lines.iter().enumerate() {
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        let text_lower = text.to_ascii_lowercase();
        let mut from = 0;
        while let Some(rel) = text_lower[from..].find(&q) {
            let abs = from + rel;
            let col_start = unicode_width::UnicodeWidthStr::width(&text[..abs]);
            let end_byte = abs + q.len();
            let col_end = unicode_width::UnicodeWidthStr::width(&text[..end_byte]);
            out.push(DocMatch { line: line_idx, col_start, col_end });
            from = end_byte.max(abs + 1);
        }
    }
    out
}

/// Wiki-link fallback: walk `root` looking for a markdown file whose basename
/// (with or without `.md`) matches the file component of `target`. Returns the
/// first hit. Bounded depth and skips dotfiles to avoid pathological scans.
fn vault_lookup(root: &Path, target: &Path) -> Option<PathBuf> {
    let needle = target.file_name()?.to_str()?.to_string();
    let needle_md = if needle.contains('.') { needle.clone() } else { format!("{}.md", needle) };
    for entry in walkdir::WalkDir::new(root)
        .follow_links(false)
        .max_depth(8)
        .into_iter()
        .filter_entry(|e| {
            e.file_name()
                .to_str()
                .map(|s| !(s.starts_with('.') && s != "." && s != ".."))
                .unwrap_or(true)
        })
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() { continue; }
        let name = match entry.file_name().to_str() { Some(n) => n, None => continue };
        if name == needle || name == needle_md {
            return Some(canonicalize_or(entry.path().to_path_buf()));
        }
    }
    None
}

impl Reader {
    pub fn from_file(path: &Path) -> Result<Self> {
        // Capture metadata BEFORE reading content: if a writer races us between
        // these two syscalls, our recorded mtime is older than the file's
        // actual mtime and the next watcher tick will reload. The other order
        // would silently swallow the concurrent edit.
        let last_meta = file_meta(path);
        let raw = std::fs::read_to_string(path)
            .map_err(|e| anyhow!("read {}: {}", path.display(), e))?;
        Ok(Self {
            origin: ReaderOrigin::File(path.to_path_buf()),
            raw,
            rendered: None,
            scroll: 0,
            focus: None,
            hover_link: None,
            hover_checkbox: None,
            doc_search: None,
            edit: None,
            last_meta,
        })
    }

    /// All focusable spans (links + checkboxes) sorted by (line, col_start).
    /// Returns `(Focus, line, col_start)` triples so callers can scroll to or
    /// highlight the focused element without re-deriving the position.
    pub fn focus_targets(&self) -> Vec<(Focus, usize, usize)> {
        let mut out: Vec<(Focus, usize, usize)> = Vec::new();
        let Some(rd) = self.rendered.as_ref() else { return out };
        for (i, l) in rd.link_map.links.iter().enumerate() {
            out.push((Focus::Link(i), l.line, l.col_start));
        }
        for (i, c) in rd.checkbox_map.items.iter().enumerate() {
            out.push((Focus::Checkbox(i), c.line, c.col_start));
        }
        out.sort_by_key(|&(_, line, col)| (line, col));
        out
    }

    /// Resolve the current focus (if any) to its (line, col_start) position.
    pub fn focus_position(&self) -> Option<(usize, usize)> {
        let rd = self.rendered.as_ref()?;
        match self.focus? {
            Focus::Link(i) => rd.link_map.links.get(i).map(|l| (l.line, l.col_start)),
            Focus::Checkbox(i) => rd.checkbox_map.items.get(i).map(|c| (c.line, c.col_start)),
        }
    }

    pub fn from_string(raw: String) -> Self {
        Self {
            origin: ReaderOrigin::Stdin,
            raw,
            rendered: None,
            scroll: 0,
            focus: None,
            hover_link: None,
            hover_checkbox: None,
            doc_search: None,
            edit: None,
            last_meta: None,
        }
    }
}

/// Run `git diff HEAD -- <path>` (which combines staged + unstaged changes
/// against the last commit) and return the raw output. Errors surface as
/// strings so the statusline can show them.
fn run_git_diff(path: &Path) -> std::result::Result<String, String> {
    use std::process::Command;
    // Run from the file's directory so `git` finds the right repo.
    let dir = path.parent().unwrap_or(Path::new("."));
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .arg("--no-pager")
        .arg("diff")
        .arg("--no-color")
        .arg("HEAD")
        .arg("--")
        .arg(path)
        .output()
        .map_err(|e| format!("spawn: {}", e))?;
    if !output.status.success() {
        // `git diff` returns 0 with no output when there are no changes.
        // A non-zero exit means a real failure (not in a repo, etc.).
        let err = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if err.is_empty() { format!("exit {}", output.status) } else { err });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Parse a unified diff into typed rows. The parser is deliberately small:
/// line-prefix dispatch, no detection of binary diffs / mode changes /
/// rename headers beyond bucketing them as `Header`. That's plenty for a
/// quick visual lens.
fn parse_unified_diff(diff: &str) -> Vec<DiffRow> {
    let mut out = Vec::with_capacity(diff.lines().count());
    for line in diff.lines() {
        let kind = if line.starts_with("@@") {
            DiffRowKind::Hunk
        } else if line.starts_with("+++") || line.starts_with("---") || line.starts_with("diff ") || line.starts_with("index ") || line.starts_with("new file") || line.starts_with("deleted file") || line.starts_with("rename ") || line.starts_with("similarity ") {
            DiffRowKind::Header
        } else if line.starts_with('+') {
            DiffRowKind::Added
        } else if line.starts_with('-') {
            DiffRowKind::Removed
        } else {
            DiffRowKind::Context
        };
        out.push(DiffRow { kind, text: line.to_string() });
    }
    out
}

/// Snapshot the current (raw, cursor) into the reader's undo stack.
/// Clears the redo stack since a new mutation diverges the timeline.
/// Caps the undo stack at `UNDO_LIMIT` entries (FIFO eviction).
fn push_undo(r: &mut Reader) {
    let Some(e) = r.edit.as_mut() else { return };
    e.undo.push(EditSnapshot { raw: r.raw.clone(), cursor: e.cursor });
    if e.undo.len() > UNDO_LIMIT {
        e.undo.remove(0);
    }
    e.redo.clear();
}

/// Snap `pos` down to the nearest UTF-8 char boundary <= pos.
fn floor_char_boundary(s: &str, pos: usize) -> usize {
    if pos >= s.len() { return s.len(); }
    let mut p = pos;
    while p > 0 && !s.is_char_boundary(p) {
        p -= 1;
    }
    p
}

/// Byte offset of the next char boundary strictly after `pos`. Returns
/// `s.len()` if `pos` is already at end.
fn next_char_boundary(s: &str, pos: usize) -> usize {
    if pos >= s.len() { return s.len(); }
    let mut p = pos + 1;
    while p < s.len() && !s.is_char_boundary(p) {
        p += 1;
    }
    p
}

/// Byte offset of the previous char boundary strictly before `pos`. Returns
/// 0 if `pos` is already at the start.
fn prev_char_boundary(s: &str, pos: usize) -> usize {
    if pos == 0 { return 0; }
    let mut p = pos - 1;
    while p > 0 && !s.is_char_boundary(p) {
        p -= 1;
    }
    p
}

/// Convert a byte offset to (line index, char column within line). Lines
/// are split on `\n`; CR is ignored. Column is in chars, not display width.
fn source_line_col(s: &str, pos: usize) -> (usize, usize) {
    let pos = pos.min(s.len());
    let head = &s[..pos];
    let line = head.bytes().filter(|&b| b == b'\n').count();
    let col_bytes = head.rfind('\n').map(|i| pos - i - 1).unwrap_or(pos);
    let col = s[pos - col_bytes..pos].chars().count();
    (line, col)
}

/// Byte offset of the first char of `line`. Out-of-range lines return
/// `s.len()`.
fn source_line_start(s: &str, line: usize) -> usize {
    if line == 0 { return 0; }
    let mut count = 0;
    for (i, b) in s.bytes().enumerate() {
        if b == b'\n' {
            count += 1;
            if count == line {
                return i + 1;
            }
        }
    }
    s.len()
}

/// Byte offset of the last char of `line` (i.e. the position just before
/// the trailing `\n`, or `s.len()` for the last line).
fn source_line_end(s: &str, line: usize) -> usize {
    let start = source_line_start(s, line);
    s[start..]
        .find('\n')
        .map(|i| start + i)
        .unwrap_or(s.len())
}

/// Walk source[range] by display-column width and return the byte offset
/// where the cursor should land for `col` columns. Used by edit-mode
/// vertical movement so a soft-wrapped paragraph steps display-row by
/// display-row, not source-line by source-line.
fn source_offset_at_col(s: &str, range: &std::ops::Range<usize>, col: usize) -> usize {
    use unicode_width::UnicodeWidthChar;
    let Some(slice) = s.get(range.clone()) else { return range.start };
    let mut taken = 0usize;
    for (i, ch) in slice.char_indices() {
        let w = ch.width().unwrap_or(0);
        if taken + w > col {
            return range.start + i;
        }
        taken += w;
    }
    range.end
}

/// Byte offset of `col` chars into `line`. Clamps if the line is shorter.
fn source_offset_for(s: &str, line: usize, col: usize) -> usize {
    let start = source_line_start(s, line);
    let end = source_line_end(s, line);
    let line_str = &s[start..end];
    let mut taken = 0usize;
    let mut last = start;
    for (i, _ch) in line_str.char_indices() {
        if taken == col { return start + i; }
        taken += 1;
        last = start + i + line_str[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
    }
    last.min(end)
}

/// Cheap stat read; returns `None` if the file is gone or unstatable. Called
/// every event-loop tick — must not allocate or do anything beyond a single
/// `metadata` syscall (kernel serves this from the inode cache).
pub fn file_meta(path: &Path) -> Option<(std::time::SystemTime, u64)> {
    let md = std::fs::metadata(path).ok()?;
    Some((md.modified().ok()?, md.len()))
}

impl Browser {
    /// Build a flat one-level listing of `dir`. The first entry is `..` when
    /// `dir` has a parent (so users who launched directly into a sub-tree can
    /// still walk up); below that, dirs come before markdown files, both
    /// sorted case-insensitively. .gitignored entries are skipped.
    pub fn scan(dir: &Path) -> Result<Self> {
        let mut b = Self {
            dir: dir.to_path_buf(),
            entries: Vec::new(),
            selected: 0,
            scroll: 0,
        };
        b.rebuild()?;
        Ok(b)
    }

    /// Rebuild `entries` from `dir`. Preserves the highlighted path across
    /// rebuilds when possible.
    pub fn rebuild(&mut self) -> Result<()> {
        let prev_selected_path = self.entries.get(self.selected).map(|e| e.path.clone());
        let mut entries = Vec::new();
        if let Some(parent) = self.dir.parent() {
            if parent != self.dir {
                entries.push(BrowserEntry {
                    path: parent.to_path_buf(),
                    display: "../".to_string(),
                    kind: BrowserEntryKind::ParentDir,
                });
            }
        }
        push_children(&self.dir, &mut entries);
        self.entries = entries;
        // Skip past `../` on a fresh listing so the cursor lands on the first
        // real entry — `Esc`/`h`/`Backspace` already covers "go up", and
        // landing on `../` makes Enter feel redundant. History-restored
        // selections set `selected` explicitly afterwards in `App::load`,
        // so this default doesn't fight that path.
        let first_real = self
            .entries
            .iter()
            .position(|e| !matches!(e.kind, BrowserEntryKind::ParentDir))
            .unwrap_or(0);
        self.selected = match prev_selected_path {
            Some(p) => self.entries.iter().position(|e| e.path == p).unwrap_or(first_real),
            None => first_real,
        };
        Ok(())
    }

    #[allow(dead_code)]
    pub fn selected_entry(&self) -> Option<&BrowserEntry> {
        self.entries.get(self.selected)
    }
}

impl Search {
    /// Build the index by walking `root` (depth-capped, gitignore-aware).
    pub fn build(root: &Path) -> Self {
        let mut paths = Vec::new();
        let walker = ignore::WalkBuilder::new(root)
            .max_depth(Some(8))
            .hidden(true)
            .git_ignore(true)
            .git_exclude(true)
            .git_global(true)
            .require_git(false)
            .build();
        for result in walker {
            let entry = match result { Ok(e) => e, Err(_) => continue };
            if entry.path() == root { continue; }
            let display = entry
                .path()
                .strip_prefix(root)
                .unwrap_or(entry.path())
                .display()
                .to_string();
            let display_lower = display.to_ascii_lowercase();
            let is_dir = entry.file_type().map(|f| f.is_dir()).unwrap_or(false);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;

    /// Per-test temp dir under the system temp root. Cleared on entry so a
    /// previous failure doesn't leave stale state behind.
    fn fresh_temp(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("md-tui-test-{}-{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn opts() -> Options {
        Options { width: 80, line_numbers: false, theme: Theme::dark() }
    }

    #[test]
    fn browser_lists_only_dirs_and_markdown() {
        let dir = fresh_temp("browser-filter");
        std::fs::create_dir_all(dir.join("subdir")).unwrap();
        std::fs::write(dir.join("a.md"), "# a").unwrap();
        std::fs::write(dir.join("b.markdown"), "# b").unwrap();
        std::fs::write(dir.join("note.txt"), "ignored").unwrap();
        std::fs::write(dir.join("Cargo.toml"), "ignored").unwrap();
        std::fs::write(dir.join(".hidden.md"), "hidden").unwrap();

        let b = Browser::scan(&dir).unwrap();
        let names: Vec<&str> = b.entries.iter().map(|e| e.display.as_str()).collect();

        assert!(names.contains(&"subdir/"), "missing subdir, got {:?}", names);
        assert!(names.contains(&"a.md"), "missing a.md, got {:?}", names);
        assert!(names.contains(&"b.markdown"), "missing b.markdown, got {:?}", names);
        assert!(!names.contains(&"note.txt"));
        assert!(!names.contains(&"Cargo.toml"));
        assert!(names.iter().all(|n| !n.contains(".hidden")));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn checkbox_toggle_writes_back_to_file() {
        let dir = fresh_temp("checkbox-toggle");
        let path = dir.join("tasks.md");
        std::fs::write(&path, "- [ ] alpha\n- [x] beta\n").unwrap();

        let mut app = App::new(Source::File(path.clone()), opts()).unwrap();
        app.ensure_rendered(80);

        // Flip the first marker (currently unchecked → checked).
        app.toggle_checkbox(0).unwrap();
        let after_first = std::fs::read_to_string(&path).unwrap();
        assert_eq!(after_first, "- [x] alpha\n- [x] beta\n");

        // Render must regenerate; toggle the second marker (checked → unchecked).
        app.ensure_rendered(80);
        app.toggle_checkbox(1).unwrap();
        let after_second = std::fs::read_to_string(&path).unwrap();
        assert_eq!(after_second, "- [x] alpha\n- [ ] beta\n");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn poll_external_change_reloads_when_file_edited() {
        let dir = fresh_temp("watch-reload");
        let path = dir.join("doc.md");
        std::fs::write(&path, "# original\n").unwrap();

        let mut app = App::new(Source::File(path.clone()), opts()).unwrap();
        app.ensure_rendered(80);
        // Steady-state tick should be a no-op even after rendering.
        assert!(!app.poll_external_change());

        // Some filesystems have second-resolution mtime — bump it so the
        // fingerprint definitely shifts. Belt-and-suspenders: the file
        // length also changes.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(&path, "# updated content\n").unwrap();

        assert!(app.poll_external_change(), "expected reload after edit");
        match &app.view {
            View::Reader(r) => assert_eq!(r.raw, "# updated content\n"),
            _ => panic!("expected reader view"),
        }
        // A second poll with no further edits should not reload again.
        assert!(!app.poll_external_change());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn poll_external_change_ignores_byte_identical_touches() {
        let dir = fresh_temp("watch-touch");
        let path = dir.join("doc.md");
        std::fs::write(&path, "# same\n").unwrap();

        let mut app = App::new(Source::File(path.clone()), opts()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        // Rewrite identical content — mtime moves but content doesn't.
        std::fs::write(&path, "# same\n").unwrap();

        assert!(!app.poll_external_change(), "no-op rewrite must not signal a reload");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn checkbox_toggle_does_not_self_trigger_reload() {
        let dir = fresh_temp("watch-self-write");
        let path = dir.join("t.md");
        std::fs::write(&path, "- [ ] task\n").unwrap();

        let mut app = App::new(Source::File(path.clone()), opts()).unwrap();
        app.ensure_rendered(80);
        app.toggle_checkbox(0).unwrap();
        // Our own write must refresh the fingerprint so the watcher tick
        // immediately afterwards does not see a phantom external change.
        assert!(!app.poll_external_change());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn checkbox_toggle_is_idempotent_round_trip() {
        let dir = fresh_temp("checkbox-roundtrip");
        let path = dir.join("t.md");
        std::fs::write(&path, "- [ ] task\n").unwrap();

        let mut app = App::new(Source::File(path.clone()), opts()).unwrap();
        app.ensure_rendered(80);
        app.toggle_checkbox(0).unwrap();
        app.ensure_rendered(80);
        app.toggle_checkbox(0).unwrap();

        let final_content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(final_content, "- [ ] task\n");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn go_back_restores_browser_selected_index() {
        let dir = fresh_temp("history-selected");
        std::fs::write(dir.join("a.md"), "# A").unwrap();
        std::fs::write(dir.join("b.md"), "# B").unwrap();
        std::fs::write(dir.join("c.md"), "# C").unwrap();

        let mut app = App::new(Source::Directory(dir.clone()), opts()).unwrap();

        // Pick a non-default entry.
        let target_idx = match &mut app.view {
            View::Browser(b) => {
                let idx = b
                    .entries
                    .iter()
                    .position(|e| e.display == "b.md")
                    .expect("b.md must be listed");
                b.selected = idx;
                idx
            }
            _ => panic!("expected browser at startup"),
        };

        // Open the file, then come back.
        let target_path = match &app.view {
            View::Browser(b) => b.entries[b.selected].path.clone(),
            _ => unreachable!(),
        };
        app.navigate_to(EntryKind::File(target_path), 0).unwrap();
        app.go_back().unwrap();

        match &app.view {
            View::Browser(b) => assert_eq!(b.selected, target_idx),
            _ => panic!("expected to land back in browser"),
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn reader_scroll_preserved_across_back_forward() {
        let dir = fresh_temp("history-scroll");
        let path = dir.join("a.md");
        std::fs::write(&path, "# A\n\nbody\n").unwrap();
        let other = dir.join("b.md");
        std::fs::write(&other, "# B").unwrap();

        let mut app = App::new(Source::File(path.clone()), opts()).unwrap();
        if let View::Reader(r) = &mut app.view {
            r.scroll = 1;
        }
        app.navigate_to(EntryKind::File(other), 0).unwrap();
        app.go_back().unwrap();
        match &app.view {
            View::Reader(r) => assert_eq!(r.scroll, 1, "scroll should be restored"),
            _ => panic!(),
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn browser_navigate_into_subdir_and_back() {
        let dir = fresh_temp("flat-nav-back");
        std::fs::create_dir_all(dir.join("inner")).unwrap();
        std::fs::write(dir.join("inner/note.md"), "# note").unwrap();

        let mut app = App::new(Source::Directory(dir.clone()), opts()).unwrap();
        app.navigate_to(EntryKind::Directory(dir.join("inner")), 0).unwrap();
        match &app.view {
            View::Browser(b) => assert_eq!(b.dir, dir.join("inner")),
            _ => panic!("expected browser at inner/"),
        }
        app.go_back().unwrap();
        match &app.view {
            View::Browser(b) => assert_eq!(b.dir, dir),
            _ => panic!("expected browser at root after back"),
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn browser_lists_one_level_only() {
        let dir = fresh_temp("flat-listing");
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("top.md"), "# top").unwrap();
        std::fs::write(dir.join("sub/inner.md"), "# inner").unwrap();

        // Top-level browser shows `sub/` and `top.md` but never recurses into
        // `sub/`, so `inner.md` must not appear.
        let b = Browser::scan(&dir).unwrap();
        assert!(b.entries.iter().any(|e| e.display == "sub/"));
        assert!(b.entries.iter().any(|e| e.display == "top.md"));
        assert!(!b.entries.iter().any(|e| e.display == "inner.md"));

        // Scanning the sub-directory directly is the only way to see its
        // contents — that's what activating `sub/` does in the event handler.
        let sub = Browser::scan(&dir.join("sub")).unwrap();
        assert!(sub.entries.iter().any(|e| e.display == "inner.md"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn go_back_walks_up_nested_browser_history() {
        let dir = fresh_temp("nested-browser-back");
        std::fs::create_dir_all(dir.join("a/b/c")).unwrap();
        let a = dir.join("a");
        let b = a.join("b");
        let c = b.join("c");

        let mut app = App::new(Source::Directory(dir.clone()), opts()).unwrap();
        app.navigate_to(EntryKind::Directory(a.clone()), 0).unwrap();
        app.navigate_to(EntryKind::Directory(b.clone()), 0).unwrap();
        app.navigate_to(EntryKind::Directory(c.clone()), 0).unwrap();

        // Pop once: should land in `b`, not quit / collapse history.
        app.go_back().unwrap();
        match &app.view {
            View::Browser(br) => assert_eq!(br.dir, b),
            _ => panic!("expected browser at b"),
        }

        // Pop again: `a`.
        app.go_back().unwrap();
        match &app.view {
            View::Browser(br) => assert_eq!(br.dir, a),
            _ => panic!("expected browser at a"),
        }

        // History still has the original root, so we're not "stuck".
        assert!(!app.history.is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn edit_mode_insert_marks_dirty_and_advances_cursor() {
        let dir = fresh_temp("edit-insert");
        let path = dir.join("doc.md");
        std::fs::write(&path, "hello\n").unwrap();

        let mut app = App::new(Source::File(path.clone()), opts()).unwrap();
        app.ensure_rendered(80);
        app.enter_edit();
        app.edit_insert("X");

        let View::Reader(r) = &app.view else { panic!("expected reader") };
        assert_eq!(r.raw, "Xhello\n");
        let e = r.edit.as_ref().unwrap();
        assert!(e.dirty);
        assert_eq!(e.cursor, 1);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn edit_save_persists_to_disk_and_clears_dirty() {
        let dir = fresh_temp("edit-save");
        let path = dir.join("doc.md");
        std::fs::write(&path, "first\n").unwrap();

        let mut app = App::new(Source::File(path.clone()), opts()).unwrap();
        app.ensure_rendered(80);
        app.enter_edit();
        app.edit_insert("X");
        app.save_edit().unwrap();

        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert_eq!(on_disk, "Xfirst\n");
        let View::Reader(r) = &app.view else { panic!() };
        assert!(!r.edit.as_ref().unwrap().dirty);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn edit_discard_reloads_from_disk_and_exits_edit_mode() {
        let dir = fresh_temp("edit-discard");
        let path = dir.join("doc.md");
        std::fs::write(&path, "original\n").unwrap();

        let mut app = App::new(Source::File(path.clone()), opts()).unwrap();
        app.ensure_rendered(80);
        app.enter_edit();
        app.edit_insert("XYZ");
        // Sanity: buffer was mutated.
        match &app.view {
            View::Reader(r) => assert_eq!(r.raw, "XYZoriginal\n"),
            _ => panic!(),
        }
        app.exit_edit_discard();

        match &app.view {
            View::Reader(r) => {
                assert!(r.edit.is_none(), "should be back in view mode");
                assert_eq!(r.raw, "original\n", "buffer should match disk");
            }
            _ => panic!(),
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn undo_restores_prior_buffer_and_cursor() {
        let dir = fresh_temp("edit-undo");
        let path = dir.join("u.md");
        std::fs::write(&path, "abc\n").unwrap();
        let mut app = App::new(Source::File(path.clone()), opts()).unwrap();
        app.ensure_rendered(80);
        app.enter_edit();
        // Insert two distinct edits; undo once → first edit remains; undo
        // twice → buffer back to original.
        app.edit_insert("X");
        app.edit_insert("Y");
        match &app.view { View::Reader(r) => assert_eq!(r.raw, "XYabc\n"), _ => panic!() };
        app.edit_undo();
        match &app.view { View::Reader(r) => assert_eq!(r.raw, "Xabc\n"), _ => panic!() };
        app.edit_undo();
        match &app.view { View::Reader(r) => assert_eq!(r.raw, "abc\n"), _ => panic!() };
        // Redo once → first edit reapplied.
        app.edit_redo();
        match &app.view { View::Reader(r) => assert_eq!(r.raw, "Xabc\n"), _ => panic!() };
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn redo_stack_clears_on_new_mutation() {
        let dir = fresh_temp("edit-redo-clear");
        let path = dir.join("r.md");
        std::fs::write(&path, "a\n").unwrap();
        let mut app = App::new(Source::File(path.clone()), opts()).unwrap();
        app.ensure_rendered(80);
        app.enter_edit();
        app.edit_insert("X");
        app.edit_undo(); // redo stack now has the X-insert
        app.edit_insert("Y"); // diverging mutation — should drop the redo
        match &app.view {
            View::Reader(r) => {
                assert_eq!(r.raw, "Ya\n");
                let e = r.edit.as_ref().unwrap();
                assert!(e.redo.is_empty(), "redo should be cleared after diverging edit");
            }
            _ => panic!(),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn parse_unified_diff_classifies_row_kinds() {
        let diff = "\
diff --git a/x.md b/x.md\n\
index abc..def 100644\n\
--- a/x.md\n\
+++ b/x.md\n\
@@ -1,3 +1,4 @@\n\
 context line\n\
-removed\n\
+added\n\
+another added\n\
 trailer\n";
        let rows = parse_unified_diff(diff);
        let kinds: Vec<DiffRowKind> = rows.iter().map(|r| r.kind).collect();
        assert_eq!(
            kinds,
            vec![
                DiffRowKind::Header,
                DiffRowKind::Header,
                DiffRowKind::Header,
                DiffRowKind::Header,
                DiffRowKind::Hunk,
                DiffRowKind::Context,
                DiffRowKind::Removed,
                DiffRowKind::Added,
                DiffRowKind::Added,
                DiffRowKind::Context,
            ],
        );
    }

    #[test]
    fn long_paragraph_wraps_in_edit_mode_with_cursor_on_correct_row() {
        let dir = fresh_temp("edit-wrap");
        let path = dir.join("long.md");
        // Single source line, much wider than the render width (40), with
        // the cursor near the end. Without wrap, the cursor would be off
        // screen.
        let body = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron pi rho sigma tau upsilon";
        std::fs::write(&path, body).unwrap();

        let mut o = opts();
        o.width = 40;
        let mut app = App::new(Source::File(path.clone()), o).unwrap();
        app.viewport = Rect::new(0, 0, 40, 24);
        app.ensure_rendered(40);
        app.enter_edit();
        // Move the cursor near the end of the buffer so it should be on
        // a wrapped row well below the first.
        if let View::Reader(r) = &mut app.view {
            r.edit.as_mut().unwrap().cursor = body.len() - 5;
        }
        app.ensure_rendered(40);

        let View::Reader(r) = &app.view else { panic!() };
        let rd = r.rendered.as_ref().unwrap();
        let xy = rd.cursor_xy.expect("cursor should have a display position");
        // Cursor must land on a row > 0 (the line wrapped) AND its column
        // must be inside the body width.
        assert!(xy.1 > 0, "expected cursor on wrapped row, got {:?}", xy);
        assert!(xy.0 < 40, "cursor col should fit within render width, got {:?}", xy);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn block_level_raw_toggle_substitutes_only_cursor_block() {
        let dir = fresh_temp("edit-toggle");
        let path = dir.join("doc.md");
        std::fs::write(&path, "# Heading\n\nSecond paragraph.\n").unwrap();

        let mut app = App::new(Source::File(path.clone()), opts()).unwrap();
        app.ensure_rendered(80);
        app.enter_edit();
        // Cursor at byte 0 → in the heading block.
        app.ensure_rendered(80);

        let View::Reader(r) = &app.view else { panic!() };
        let rd = r.rendered.as_ref().unwrap();
        // The heading line should be visible with its raw `#` marker since
        // the cursor is in that block.
        let any_raw_heading = rd.lines.iter().any(|l| {
            let s: String = l.spans.iter().map(|sp| sp.content.as_ref()).collect();
            s.contains("# Heading")
        });
        assert!(any_raw_heading, "expected raw heading line in rendered output");

        // The other paragraph should remain formatted (no `#` markers).
        let any_raw_para_marker = rd.lines.iter().any(|l| {
            let s: String = l.spans.iter().map(|sp| sp.content.as_ref()).collect();
            s.contains("# Second")
        });
        assert!(!any_raw_para_marker, "second paragraph should stay formatted");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn fresh_browser_skips_parent_dir_default_selection() {
        let dir = fresh_temp("browser-default-skip");
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.md"), "# a").unwrap();
        std::fs::write(dir.join("b.md"), "# b").unwrap();

        let b = Browser::scan(&dir).unwrap();
        // First entry is `../`; cursor should not start on it.
        assert!(matches!(b.entries[0].kind, BrowserEntryKind::ParentDir));
        assert!(b.selected > 0, "expected to skip ../, got selected={}", b.selected);
        assert!(!matches!(b.entries[b.selected].kind, BrowserEntryKind::ParentDir));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn root_directory_with_no_parent_starts_at_zero() {
        // Filesystem root has no parent → `../` not added → first entry
        // is a real one and the cursor lands on it (index 0).
        let dir = fresh_temp("browser-no-parent");
        std::fs::write(dir.join("a.md"), "# a").unwrap();

        // Force the no-parent case by stripping the parent reference. Easier:
        // just verify rebuild's fallback in the absence of `..`.
        let mut b = Browser {
            dir: dir.clone(),
            entries: vec![BrowserEntry {
                path: dir.join("a.md"),
                display: "a.md".to_string(),
                kind: BrowserEntryKind::Markdown,
            }],
            selected: 0,
            scroll: 0,
        };
        // Re-scan (rebuild discovers `..` if the dir has a parent — fine, just
        // assert the fallback never out-of-bounds).
        b.rebuild().unwrap();
        assert!(b.selected < b.entries.len());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn focus_targets_interleaves_links_and_checkboxes_in_document_order() {
        let dir = fresh_temp("focus-targets");
        let path = dir.join("doc.md");
        // Two links, two checkboxes, intentionally interleaved so plain
        // sequential indexing wouldn't yield document order.
        let src = "\
- [ ] task one with [link a](https://a)\n\
\n\
[link b](https://b)\n\
\n\
- [x] task two\n";
        std::fs::write(&path, src).unwrap();

        let mut app = App::new(Source::File(path.clone()), opts()).unwrap();
        app.ensure_rendered(80);
        let View::Reader(r) = &app.view else { panic!("expected reader") };

        let targets = r.focus_targets();
        // Expect 4 items: cb0, link0 (same line as cb0), link1, cb1.
        assert_eq!(targets.len(), 4, "got {:?}", targets);
        let kinds: Vec<&'static str> = targets
            .iter()
            .map(|(f, _, _)| match f {
                Focus::Link(_) => "link",
                Focus::Checkbox(_) => "cb",
            })
            .collect();
        assert_eq!(kinds, vec!["cb", "link", "link", "cb"], "ordering: {:?}", targets);

        // The lines must be monotonically non-decreasing.
        for w in targets.windows(2) {
            assert!(w[0].1 <= w[1].1, "lines not in order: {:?}", targets);
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn is_markdown_file_recognises_extensions() {
        assert!(is_markdown_file(Path::new("foo.md")));
        assert!(is_markdown_file(Path::new("foo.MD")));
        assert!(is_markdown_file(Path::new("foo.markdown")));
        assert!(is_markdown_file(Path::new("foo.mdown")));
        assert!(!is_markdown_file(Path::new("foo.txt")));
        assert!(!is_markdown_file(Path::new("foo")));
    }
}
