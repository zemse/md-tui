# Design: "Unread" tracking for browsed files

Goal: a freshly-created (or freshly-changed) markdown file shows an `[unread]`
badge in the browser. The badge clears once the user **opens the file and
scrolls to the bottom**, or explicitly marks it read. Directories show
`[unread]` when they contain any unread descendant.

**Decision (chosen): Option C — mtime-based**, with a tiny per-file store so
manual/recursive mark-read works (see §1). No content hashing, no wall-clock
dependency.

This doc is the spec; behaviour is not implemented yet.

---

## The three orthogonal decisions

1. **Where read-state lives** (persistence)
2. **What "unread" means** (the predicate + parent propagation)
3. **When a file flips to read** (the trigger)

---

## 1. Where read-state lives — Option C (mtime), chosen

A file is *read* if we have recorded the mtime it had when it was read, and the
file's current mtime still matches. Otherwise it is *unread*.

```
unread(file) = no record  OR  current_mtime(file) != recorded_mtime
```

Why mtime as the marker (vs a content hash): cheap (a `stat`, no file read/hash),
and it needs **no clock** — we store the file's own mtime value, not "now". When
`/research` regenerates a report, its mtime changes ⇒ it auto-reverts to
`[unread]`. The known weakness is that `git checkout` rewrites mtimes, so a fresh
clone can show files as unread; acceptable, and the seed-as-read bootstrap (§2)
plus the `r` keybind (§4) both paper over it.

### Why a per-file store (not a single global timestamp)
The literal "Option C" sketch — one global `last_browsed` timestamp, zero
per-file state — cannot express a per-file or per-dir **mark read**: there'd be
nothing to flip for a single entry. Since we want the recursive `r` shortcut
(§4) and scroll-to-bottom auto-marking (§3), we keep a minimal map:

```
path -> recorded_mtime
```

Stored as JSON in md-tui's state dir (mirror `config.rs`'s `dirs::config_dir()`
pattern, but prefer `dirs::state_dir()`):
- Linux: `~/.local/state/md/read.json`
- macOS: `~/Library/Application Support/md/read.json`

```json
{
  "/abs/path/to/research/foo/report.md": { "mtime": 1733500200 }
}
```

- **+** No pollution of research dirs, no git churn, generalizes to all md-tui use.
- **−** Machine-local (no cross-machine sync); absolute-path keys break on repo move.
  Both acceptable for now; a repo-local JSON backend could be added later behind a
  config flag without changing the in-memory model.

(Rejected alternatives — content-hash marker: more robust but needless hashing on
every browsed file; xattrs: dropped by git/copy tools; frontmatter/sidecar:
mutates content. See git history of this file for the fuller comparison.)

---

## 2. What "unread" means + parent propagation

### File predicate
`unread(file) = state.get(path).map_or(true, |m| current_mtime(file) != m)`

### The bootstrap problem (important)
First run has an empty state ⇒ **every existing file shows unread**, contradicting
"only *new* researches are unread". On first init (no state file present), walk
the tree once and record current mtimes for all md files — a seed-as-read
snapshot. Only files created/changed afterward then appear unread.

### Parent directory propagation
The browser is **one level deep** (`push_children`, `app.rs`). For each `Dir`
entry in the current view, compute "contains an unread descendant" by walking it
with the existing `ignore::WalkBuilder` and testing `unread()` on each md file;
short-circuit on first hit. Research dirs are small, so this is fine; cache
`dir -> has_unread` and invalidate on `rebuild()` / mark-read if it ever bites.
Leave the `../` (ParentDir) entry unbadged.

---

## 3. When a file flips to read — the trigger

Mark read the moment the reader's viewport reaches the end of the document:

```rust
let total = rendered.lines.len();
let reached_bottom = r.scroll as usize + app.viewport.height as usize >= total;
```

- Files shorter than the viewport satisfy this **on open** ⇒ read immediately
  (nothing to scroll). Longer files require scrolling to the last line.
- Hook the check in `scroll_by`/`scroll_to` (events.rs) and once in `draw_reader`
  (ui.rs) for the short-file-on-open case. On first satisfaction:
  `state[path] = current_mtime(path)`; mark store dirty.
- **Persistence cadence:** update the map in memory immediately; flush JSON on
  mark-read and on quit (dirty flag avoids rewriting on every keypress).

---

## 4. Keybind: `r` — mark read (browser only)  ✅ confirmed free

`r` (plain) is currently **unbound**; only `Ctrl+r` is used (edit-mode redo), so
no conflict. Key dispatch is a single shared `match key.code` in
`events.rs:163`; add an arm that acts only in `View::Browser`:

```rust
KeyCode::Char('r') => {
    if let View::Browser(b) = &app.view {
        let path = b.entries[b.selected].path.clone(); // guard empty/ParentDir
        app.read_state.mark_read_recursive(&path);     // file: one entry; dir: all descendants
        if let View::Browser(b) = &mut app.view { b.rebuild()?; } // refresh badges
        app.read_state.flush();
    }
}
```

Semantics of `r` on the selected entry:
- **Markdown file** → record its current mtime (clears its `[unread]`).
- **Directory** → recurse with `ignore::WalkBuilder`, record current mtime for
  **every** descendant md file. This clears the dir's badge (derived from
  descendants) and all unread files/subdirs under it.
- **`../` / ParentDir** → no-op (don't let the user accidentally mark the whole
  parent subtree).

(Optional companion later: `R` or a modifier to mark **unread** = drop the entry
from the map.)

---

## Implementation sketch

New module `src/read_state.rs`:
```rust
pub struct ReadState { map: HashMap<PathBuf, u64 /* mtime secs */>, dirty: bool }

impl ReadState {
    pub fn load() -> Self;                         // state dir; seed-as-read if file absent
    pub fn is_unread(&self, path: &Path) -> bool;  // miss OR mtime mismatch
    pub fn dir_has_unread(&self, dir: &Path) -> bool; // ignore-walk, short-circuit
    pub fn mark_read(&mut self, path: &Path);      // record current mtime, set dirty
    pub fn mark_read_recursive(&mut self, path: &Path); // file -> mark_read; dir -> walk+mark each md
    pub fn flush(&self);                           // write JSON if dirty
}
```

Wiring:
1. `BrowserEntry` → add `unread: bool`; fill in `push_children`/`rebuild`
   (`is_unread` for files, `dir_has_unread` for dirs). `app.rs`
2. `App` → own a `ReadState`; thread into `Browser::rebuild`. `app.rs`
3. `draw_browser` → append a styled `" [unread]"` to `e.display`. `ui.rs` ~L737
4. Bottom-detection auto-mark in `scroll_by`/`scroll_to` + `draw_reader`. `events.rs`/`ui.rs`
5. `r` keybind arm (browser-only) as above. `events.rs:163`
6. `flush()` on mark-read and on app exit. `main.rs`/event loop

No new crate needed (uses `std::fs::metadata().modified()` + existing `serde_json`,
`ignore`, `dirs`).

### Nice-to-haves (later)
- Config toggle `track_unread = true|false`.
- `R`/modifier to mark unread; mark-all-read.
- Distinguish "never opened" vs "changed since read" badges (`[unread]` vs `[updated]`).
- Sort-unread-first or filter to unread only.

---

## Status: implemented

Shipped in `src/read_state.rs` (`ReadState`). Deviations from the sketch above,
all simplifications:

- **State file:** `<state-or-data-dir>/md/read.state` — one `mtime<TAB>path` line
  per entry, **not** JSON. No new crate: hand-rolled parse/format, so `serde_json`
  was never needed. (`dirs::state_dir()` is `Some` only on Linux; macOS falls back
  to `dirs::data_dir()` → `~/Library/Application Support/md/`.)
- **Badges are computed at render time** in `draw_browser` (ui.rs), not stored on
  `BrowserEntry`. So `is_unread` / `dir_has_unread` run live each frame and need no
  cache-invalidation; pressing `r` or scrolling a file to the bottom updates badges
  on the very next frame with no `rebuild()`. (Revisit with a cache only if a large
  tree ever makes the per-frame recursive walk noticeable.)
- **Auto mark-read lives entirely in `draw_reader`** (ui.rs): when the last line is
  on screen (`scroll + viewport_height >= total`), record the file's mtime. This one
  spot covers both "short file, read on open" and "scrolled to the end of a long
  file" — no hook needed in `scroll_by`/`scroll_to`. Suppressed in edit mode.
- **`r` keybind:** added to the shared key match in events.rs. File → mark it read;
  dir → recurse and mark every text file under it; `../` → no-op. Flushes immediately.
- **Persistence:** `flush()` on the `r` action and once on app exit (`main.rs`);
  first-run seeding flushes its snapshot too.

Verified: first run seeds all existing files (recursively) as read; a later run does
not re-seed, so a newly-created file is absent from the state and shows `[unread]`.
`cargo build` + `clippy` clean on the new code; all 66 tests pass.
