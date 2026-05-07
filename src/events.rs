use std::io::stdout;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::execute;

use crate::app::{self, App, BrowserEntry, BrowserEntryKind, EntryKind, Focus, SearchResult, View};
use crate::ui;

/// Two-key vim chord (g, z) timeout.
const CHORD_TIMEOUT_MS: u128 = 700;

/// Take the buffered numeric prefix (or `1` if none), clamped to a sane
/// minimum so motion arms always make progress.
fn consume_count(prefix: &mut Option<u32>) -> i32 {
    prefix.take().unwrap_or(1).max(1) as i32
}

pub fn run(term: &mut ui::Term, app: &mut App) -> Result<()> {
    while !app.should_quit {
        // Detect external edits to the open file. One stat syscall per tick
        // (≤4/sec when idle, served from the kernel inode cache) — far
        // cheaper than a notification thread, and zero new dependencies.
        app.poll_external_change();
        term.draw(|f| ui::draw(f, app))?;
        if !event::poll(Duration::from_millis(250))? { continue; }
        match event::read()? {
            Event::Key(k) if k.kind == KeyEventKind::Press => handle_key(app, k)?,
            Event::Mouse(m) => handle_mouse(app, m)?,
            Event::Resize(_, _) => {
                if let View::Reader(r) = &mut app.view {
                    r.rendered = None;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn handle_key(app: &mut App, key: KeyEvent) -> Result<()> {
    // Any key press hides the mouse-driven cursor suppression so the keyboard
    // focus highlight reappears.
    app.mouse_recent = false;

    // Ctrl+C is a hard exit no matter what overlay is on screen.
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        app.should_quit = true;
        return Ok(());
    }
    if app.help_open {
        match key.code {
            KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') => app.help_open = false,
            _ => {}
        }
        return Ok(());
    }
    if app.search.is_some() {
        return handle_search_key(app, key);
    }
    if let View::Reader(r) = &app.view {
        if r.doc_search.as_ref().map(|s| s.editing).unwrap_or(false) {
            return handle_doc_search_key(app, key);
        }
    }

    // Time-out stale chord state before interpreting the next key.
    let now = std::time::Instant::now();
    if let Some(t) = app.pending_g {
        if now.duration_since(t).as_millis() > CHORD_TIMEOUT_MS {
            app.pending_g = None;
        }
    }
    if let Some(t) = app.pending_z {
        if now.duration_since(t).as_millis() > CHORD_TIMEOUT_MS {
            app.pending_z = None;
        }
    }

    // Resolve `gg` / `zz` chord completions before falling into the main match.
    if app.pending_g.is_some() {
        app.pending_g = None;
        if let KeyCode::Char('g') = key.code {
            scroll_to(app, 0);
            app.count_prefix = None;
            return Ok(());
        }
        // Anything else: cancel the chord and fall through to normal handling.
    }
    if app.pending_z.is_some() {
        app.pending_z = None;
        if let KeyCode::Char('z') = key.code {
            center_focus_or_top(app);
            app.count_prefix = None;
            return Ok(());
        }
    }

    // Numeric prefix: digits are buffered into `count_prefix` until a motion
    // key consumes them. Plain `0` resets the count *only* if no count is
    // already being typed (matches vim — `0` on its own is "go to col 0",
    // which doesn't apply to a viewer).
    if let KeyCode::Char(c) = key.code {
        if !key.modifiers.contains(KeyModifiers::CONTROL)
            && c.is_ascii_digit()
            && (c != '0' || app.count_prefix.is_some())
        {
            let d = c.to_digit(10).unwrap();
            let cur = app.count_prefix.unwrap_or(0);
            // Cap at 99,999 to keep behavior sane on accidental key-mash.
            let next = (cur.saturating_mul(10)).saturating_add(d).min(99_999);
            app.count_prefix = Some(next);
            return Ok(());
        }
    }

    match key.code {
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Esc => {
            // Cancel any in-progress count or chord first.
            if app.count_prefix.is_some() || app.pending_g.is_some() || app.pending_z.is_some() {
                app.count_prefix = None;
                app.pending_g = None;
                app.pending_z = None;
                return Ok(());
            }
            // Dismiss any committed in-doc search overlay.
            if let View::Reader(r) = &app.view {
                if r.doc_search.is_some() {
                    app.close_doc_search();
                    return Ok(());
                }
            }
            if !app.history.is_empty() {
                app.go_back()?;
            } else {
                app.should_quit = true;
            }
        }
        KeyCode::Char('?') => app.help_open = !app.help_open,
        KeyCode::Char('/') => match &app.view {
            View::Reader(_) => app.open_doc_search(),
            View::Browser(_) => app.open_search(),
        },
        KeyCode::Char('T') => app.open_search(),
        KeyCode::Char('n') => app.doc_search_step(true),
        KeyCode::Char('N') => app.doc_search_step(false),
        KeyCode::Char('m') => toggle_mouse(app),
        KeyCode::Char('e') => edit_current_file(app)?,

        // Vim-style scrolling. Ctrl-modified arms come *before* unguarded
        // letter arms so the modifier path can match.
        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            let n = consume_count(&mut app.count_prefix);
            scroll_by_page(app, n, true);
        }
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            let n = consume_count(&mut app.count_prefix);
            scroll_by_page(app, -n, true);
        }
        KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            let n = consume_count(&mut app.count_prefix);
            scroll_by_page(app, n, false);
        }
        KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            let n = consume_count(&mut app.count_prefix);
            scroll_by_page(app, -n, false);
        }

        // History (plain `b`/`f`). These don't consume the count.
        KeyCode::Char('b') | KeyCode::Backspace => app.go_back()?,
        KeyCode::Char('f') => app.go_forward()?,

        KeyCode::Char('j') | KeyCode::Down => {
            let n = consume_count(&mut app.count_prefix);
            scroll_by(app, n);
        }
        KeyCode::Char('k') | KeyCode::Up => {
            let n = consume_count(&mut app.count_prefix);
            scroll_by(app, -n);
        }
        // Lowercase shortcuts kept for parity with prior bindings.
        KeyCode::Char('d') => {
            let n = consume_count(&mut app.count_prefix);
            scroll_by_page(app, n, true);
        }
        KeyCode::Char('u') => {
            let n = consume_count(&mut app.count_prefix);
            scroll_by_page(app, -n, true);
        }
        KeyCode::PageDown | KeyCode::Char(' ') => {
            let n = consume_count(&mut app.count_prefix);
            scroll_by_page(app, n, false);
        }
        KeyCode::PageUp => {
            let n = consume_count(&mut app.count_prefix);
            scroll_by_page(app, -n, false);
        }

        // `g` is now the first key of `gg`. The count is preserved for the
        // chord completion to consume.
        KeyCode::Char('g') => {
            app.pending_g = Some(std::time::Instant::now());
        }
        KeyCode::Char('G') | KeyCode::End => {
            let n = consume_count(&mut app.count_prefix);
            if n > 1 {
                scroll_to(app, (n - 1).max(0) as u16);
            } else {
                scroll_to(app, u16::MAX);
            }
        }
        KeyCode::Home => scroll_to(app, 0),

        // `z` is the first key of `zz` (center on focus). Lowercase only —
        // uppercase Z is unbound.
        KeyCode::Char('z') => {
            app.pending_z = Some(std::time::Instant::now());
        }

        // Viewport-relative jumps (vim H/M/L).
        KeyCode::Char('H') => scroll_viewport_relative(app, ViewportTarget::Top),
        KeyCode::Char('M') => scroll_viewport_relative(app, ViewportTarget::Middle),
        KeyCode::Char('L') => scroll_viewport_relative(app, ViewportTarget::Bottom),

        // Focus walks links AND checkboxes.
        KeyCode::Tab => focus_next(app),
        KeyCode::BackTab => focus_prev(app),
        KeyCode::Enter => activate(app)?,
        KeyCode::Char('o') => open_focused(app)?,

        // Browser navigation arrows.
        KeyCode::Right => enter_or_open(app)?,
        KeyCode::Left => {
            if matches!(app.view, View::Browser(_)) { app.go_back()?; }
        }
        // `h`/`l` only walk history when not in a count chord. (No conflict
        // with `gh` etc. since we resolved `pending_g` above already.)
        KeyCode::Char('h') => app.go_back()?,
        KeyCode::Char('l') => app.go_forward()?,

        _ => {}
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum ViewportTarget { Top, Middle, Bottom }

/// Vim H/M/L: place the focused element (or the visible top/middle/bottom
/// content line) without changing the underlying buffer position. Here, since
/// we have no separate "cursor line" beyond focus, H/M/L instead pick a focus
/// target visible on that part of the viewport.
fn scroll_viewport_relative(app: &mut App, target: ViewportTarget) {
    let h = app.viewport.height as usize;
    if let View::Reader(r) = &mut app.view {
        let Some(rendered) = r.rendered.as_ref() else { return; };
        let scroll = r.scroll as usize;
        let last = rendered.lines.len().saturating_sub(1);
        let want_line = match target {
            ViewportTarget::Top => scroll,
            ViewportTarget::Middle => (scroll + h / 2).min(last),
            ViewportTarget::Bottom => (scroll + h.saturating_sub(1)).min(last),
        };
        // Pick the focusable nearest to that line, if any.
        let mut targets = r.focus_targets();
        if targets.is_empty() { return; }
        targets.sort_by_key(|&(_, line, _)| (line as i64 - want_line as i64).abs());
        r.focus = Some(targets[0].0);
    }
}

/// Center the focus line in the viewport. Falls back to centering the top of
/// the buffer when nothing is focused.
fn center_focus_or_top(app: &mut App) {
    let h = app.viewport.height as usize;
    if let View::Reader(r) = &mut app.view {
        let line = r.focus_position().map(|(l, _)| l).unwrap_or(0);
        let total = r.rendered.as_ref().map(|x| x.lines.len()).unwrap_or(0);
        let max_scroll = total.saturating_sub(h);
        r.scroll = line.saturating_sub(h / 2).min(max_scroll) as u16;
    }
}

/// Suspend the TUI, hand the terminal to `$EDITOR` (or `vi`) on the current
/// reader file, then restore raw mode and reload the file. No-op for stdin.
fn edit_current_file(app: &mut App) -> Result<()> {
    use crossterm::execute;
    use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode};

    let path = match &app.view {
        View::Reader(r) => match &r.origin {
            crate::app::ReaderOrigin::File(p) => p.clone(),
            crate::app::ReaderOrigin::Stdin => {
                app.status = "Cannot edit: source is stdin".into();
                return Ok(());
            }
        },
        _ => return Ok(()),
    };

    let editor = std::env::var("EDITOR")
        .or_else(|_| std::env::var("VISUAL"))
        .unwrap_or_else(|_| "vi".to_string());

    // Restore the terminal so the editor has full control.
    disable_raw_mode().ok();
    let mut out = stdout();
    execute!(out, LeaveAlternateScreen, DisableMouseCapture).ok();

    let status = std::process::Command::new(&editor).arg(&path).status();

    // Restore TUI state in all cases.
    enable_raw_mode().ok();
    execute!(out, EnterAlternateScreen).ok();
    if app.mouse_enabled {
        execute!(out, EnableMouseCapture).ok();
    }

    match status {
        Ok(s) if s.success() => {
            // Reload the file from disk so any edits are reflected.
            if let View::Reader(r) = &mut app.view {
                if let Ok(raw) = std::fs::read_to_string(&path) {
                    r.raw = raw;
                    r.rendered = None;
                    app.status = format!("Reloaded {}", editor);
                }
            }
        }
        Ok(_) => app.status = format!("{} exited non-zero", editor),
        Err(e) => app.status = format!("{}: {}", editor, e),
    }
    Ok(())
}

/// Toggle mouse capture so the user can drag-select text natively. When
/// capture is on we get scroll/click/hover; when off the terminal handles
/// dragging.
fn toggle_mouse(app: &mut App) {
    let mut out = stdout();
    if app.mouse_enabled {
        let _ = execute!(out, DisableMouseCapture);
        app.mouse_enabled = false;
        app.status = "Mouse off — drag to select text (m to re-enable)".into();
    } else {
        let _ = execute!(out, EnableMouseCapture);
        app.mouse_enabled = true;
        app.status = "Mouse on".into();
    }
}

/// Handle keystrokes while the in-document search prompt is open.
fn handle_doc_search_key(app: &mut App, key: KeyEvent) -> Result<()> {
    // Ctrl+C always quits, even with overlays open.
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        app.should_quit = true;
        return Ok(());
    }
    match key.code {
        KeyCode::Esc => app.close_doc_search(),
        KeyCode::Enter => app.doc_search_commit(),
        KeyCode::Backspace => {
            if let View::Reader(r) = &mut app.view {
                if let Some(s) = &mut r.doc_search { s.query.pop(); }
            }
            app.doc_search_refresh();
        }
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            if let View::Reader(r) = &mut app.view {
                if let Some(s) = &mut r.doc_search { s.query.clear(); }
            }
            app.doc_search_refresh();
        }
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            if let View::Reader(r) = &mut app.view {
                if let Some(s) = &mut r.doc_search { s.query.push(c); }
            }
            app.doc_search_refresh();
        }
        _ => {}
    }
    Ok(())
}

fn handle_search_key(app: &mut App, key: KeyEvent) -> Result<()> {
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        app.should_quit = true;
        return Ok(());
    }
    match key.code {
        KeyCode::Esc => app.close_search(),
        KeyCode::Enter => {
            let result = app
                .search
                .as_ref()
                .and_then(|s| s.results.get(s.selected).cloned());
            app.close_search();
            if let Some(r) = result {
                open_search_result(app, r)?;
            }
        }
        KeyCode::Down | KeyCode::Tab => {
            if let Some(s) = &mut app.search { s.move_selection(1); }
        }
        KeyCode::Up | KeyCode::BackTab => {
            if let Some(s) = &mut app.search { s.move_selection(-1); }
        }
        KeyCode::PageDown => {
            if let Some(s) = &mut app.search { s.move_selection(10); }
        }
        KeyCode::PageUp => {
            if let Some(s) = &mut app.search { s.move_selection(-10); }
        }
        KeyCode::Backspace => {
            if let Some(s) = &mut app.search {
                s.query.pop();
                s.refresh();
            }
        }
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            if let Some(s) = &mut app.search {
                s.query.clear();
                s.refresh();
            }
        }
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            if let Some(s) = &mut app.search {
                s.query.push(c);
                s.refresh();
            }
        }
        _ => {}
    }
    Ok(())
}

fn open_search_result(app: &mut App, r: SearchResult) -> Result<()> {
    if r.is_dir {
        app.navigate_to(EntryKind::Directory(r.path), 0)?;
    } else if app::is_markdown_file(&r.path) {
        app.navigate_to(EntryKind::File(r.path), 0)?;
    } else {
        let _ = open::that_detached(&r.path);
        app.status = format!("Opened externally: {}", r.path.display());
    }
    Ok(())
}

fn handle_mouse(app: &mut App, m: MouseEvent) -> Result<()> {
    // Anything other than scroll/move counts as deliberate mouse interaction
    // and re-engages the mouse-cursor mode (hides the keyboard focus halo).
    match m.kind {
        MouseEventKind::Moved
        | MouseEventKind::Drag(_)
        | MouseEventKind::Down(_)
        | MouseEventKind::Up(_)
        | MouseEventKind::ScrollUp
        | MouseEventKind::ScrollDown => {
            app.mouse_recent = true;
            app.last_mouse_col = m.column;
            app.last_mouse_row = m.row;
        }
        _ => {}
    }

    // Statusline: clickable back button. (Was in the dedicated header row;
    // since we collapsed the layout it now sits on the statusline.)
    if m.row == app.statusline_area.y && matches!(m.kind, MouseEventKind::Down(MouseButton::Left)) {
        if let Some((sx, ex)) = app.back_button_hit {
            if m.column >= sx && m.column < ex {
                if !app.history.is_empty() {
                    app.go_back()?;
                }
                return Ok(());
            }
        }
    }
    // While the search overlay is up, the mouse wheel scrolls results.
    if app.search.is_some() {
        match m.kind {
            MouseEventKind::ScrollUp => {
                if let Some(s) = &mut app.search { s.move_selection(-1); }
            }
            MouseEventKind::ScrollDown => {
                if let Some(s) = &mut app.search { s.move_selection(1); }
            }
            _ => {}
        }
        return Ok(());
    }
    let area = app.viewport;
    if !point_in(area, m.column, m.row) {
        match m.kind {
            MouseEventKind::ScrollUp => wheel_scroll(app, -3),
            MouseEventKind::ScrollDown => wheel_scroll(app, 3),
            _ => {}
        }
        return Ok(());
    }
    match m.kind {
        MouseEventKind::ScrollUp => wheel_scroll(app, -3),
        MouseEventKind::ScrollDown => wheel_scroll(app, 3),
        MouseEventKind::Moved => {
            update_hover(app, m.column, m.row);
        }
        MouseEventKind::Down(MouseButton::Left) => {
            // Double-click → select & copy the word under the cursor.
            let now = std::time::Instant::now();
            let double = match app.last_click {
                Some((t, c, r)) => {
                    now.duration_since(t) <= std::time::Duration::from_millis(450)
                        && c == m.column
                        && r == m.row
                }
                None => false,
            };
            app.last_click = Some((now, m.column, m.row));
            if double {
                select_word_at(app, m.column, m.row);
            } else {
                click_at(app, m.column, m.row)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Select the word under the click and push it to the system clipboard.
/// Uses `pbcopy` on macOS (always works locally) and OSC 52 elsewhere /
/// as a fallback so it still works over SSH. Updates the status bar.
fn select_word_at(app: &mut App, col: u16, row: u16) {
    let area = app.viewport;
    if !point_in(area, col, row) { return; }
    let View::Reader(r) = &app.view else { return; };
    let Some(rendered) = &r.rendered else { return; };

    let line_num_w = if app.opts.line_numbers {
        (format!("{}", rendered.lines.len()).len() + 1) as u16
    } else { 0 };
    let inner_x = area.x + line_num_w;
    if col < inner_x { return; }
    let local_col = (col - inner_x) as usize;
    let line_idx = r.scroll as usize + (row - area.y) as usize;
    let Some(line) = rendered.lines.get(line_idx) else { return; };

    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    let Some(word) = word_at_col(&text, local_col) else { return; };

    copy_to_clipboard(&word);
    app.status = format!("Copied: {}", word);
}

/// Best-effort copy: native helper on macOS, OSC 52 otherwise (with tmux
/// passthrough wrapping when applicable). Both paths are silent on failure —
/// the status line already reports what we attempted to copy.
fn copy_to_clipboard(text: &str) {
    #[cfg(target_os = "macos")]
    {
        use std::io::Write;
        use std::process::{Command, Stdio};
        if let Ok(mut child) = Command::new("pbcopy").stdin(Stdio::piped()).spawn() {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            let _ = child.wait();
            return;
        }
    }
    osc52_copy(text);
}

fn osc52_copy(text: &str) {
    use std::io::Write;
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    let mut out = stdout();
    if std::env::var_os("TMUX").is_some() {
        // tmux DCS passthrough: tmux strips the wrapper and forwards the inner
        // OSC 52 to the outer terminal.
        let _ = write!(out, "\x1bPtmux;\x1b\x1b]52;c;{}\x07\x1b\\", encoded);
    } else {
        let _ = write!(out, "\x1b]52;c;{}\x1b\\", encoded);
    }
    let _ = out.flush();
}

#[cfg(test)]
mod scroll_damp_tests {
    use super::compute_dampened_scroll;
    use std::time::{Duration, Instant};

    #[test]
    fn first_event_passes_through_at_full_strength() {
        let mut last = None;
        let mut accum = 0.0;
        let d = compute_dampened_scroll(&mut last, &mut accum, 3, Instant::now());
        assert_eq!(d, 3);
    }

    #[test]
    fn slow_scroll_keeps_full_speed() {
        let mut last = None;
        let mut accum = 0.0;
        let t0 = Instant::now();
        assert_eq!(compute_dampened_scroll(&mut last, &mut accum, 3, t0), 3);
        // 250ms later — past the burst threshold; full delta passes through.
        let t1 = t0 + Duration::from_millis(250);
        assert_eq!(compute_dampened_scroll(&mut last, &mut accum, 3, t1), 3);
    }

    #[test]
    fn rapid_burst_is_halved_overall() {
        let mut last = None;
        let mut accum = 0.0;
        let t0 = Instant::now();
        // First event sets the clock; treat it as the start of the burst.
        let _ = compute_dampened_scroll(&mut last, &mut accum, 3, t0);
        // Six follow-up events 50ms apart — all in the burst zone (factor 0.5).
        let mut total = 0;
        for i in 1..=6 {
            let t = t0 + Duration::from_millis(50 * i);
            total += compute_dampened_scroll(&mut last, &mut accum, 3, t);
        }
        // Raw would have been 6 * 3 = 18 lines; dampened ≈ 9.
        assert_eq!(total, 9, "burst of 6 events at 3 lines should yield ~9 dampened");
    }

    #[test]
    fn fractional_accumulator_is_preserved() {
        let mut last = None;
        let mut accum = 0.0;
        let t0 = Instant::now();
        let _ = compute_dampened_scroll(&mut last, &mut accum, 3, t0);
        // 50ms later: 3 * 0.5 = 1.5 lines → 1 line emitted, 0.5 carried.
        let t1 = t0 + Duration::from_millis(50);
        assert_eq!(compute_dampened_scroll(&mut last, &mut accum, 3, t1), 1);
        // 50ms later again: 1.5 + 0.5 carry = 2.0 lines → 2 emitted, 0 carried.
        let t2 = t1 + Duration::from_millis(50);
        assert_eq!(compute_dampened_scroll(&mut last, &mut accum, 3, t2), 2);
    }

    #[test]
    fn long_pause_resets_accumulator() {
        let mut accum = 0.7;
        let now = Instant::now();
        // Pretend the last event was way back; elapsed > 500ms.
        let mut last = Some(now - Duration::from_secs(2));
        let _ = compute_dampened_scroll(&mut last, &mut accum, 3, now);
        // Accum should have been zeroed before this event's contribution.
        // After a 2-second gap, factor=1.0, so 3 lines emitted, accum back to 0.
        assert!(accum.abs() < 1e-4, "accum should reset on long pause, got {}", accum);
    }

    #[test]
    fn direction_reversal_resets_carry() {
        let mut last = None;
        let mut accum = 0.0;
        let t0 = Instant::now();
        let _ = compute_dampened_scroll(&mut last, &mut accum, 3, t0);
        // Build positive carry.
        let t1 = t0 + Duration::from_millis(50);
        let _ = compute_dampened_scroll(&mut last, &mut accum, 3, t1);
        assert!(accum > 0.0);
        // Reverse direction — leftover positive carry must not cancel the
        // first up-scroll line.
        let t2 = t1 + Duration::from_millis(50);
        let d = compute_dampened_scroll(&mut last, &mut accum, -3, t2);
        assert!(d <= -1, "reversal should still emit a line in the new direction, got {}", d);
    }
}

#[cfg(test)]
mod word_tests {
    use super::word_at_col;

    #[test]
    fn picks_word_in_simple_line() {
        let line = "the quick brown fox";
        // 'q' lives at columns 4..5
        assert_eq!(word_at_col(line, 4).as_deref(), Some("quick"));
        assert_eq!(word_at_col(line, 6).as_deref(), Some("quick"));
    }

    #[test]
    fn returns_none_on_whitespace() {
        let line = "alpha   beta";
        assert_eq!(word_at_col(line, 6), None);
    }

    #[test]
    fn strips_trailing_punctuation() {
        let line = "hello, world!";
        assert_eq!(word_at_col(line, 0).as_deref(), Some("hello"));
        assert_eq!(word_at_col(line, 8).as_deref(), Some("world"));
    }

    #[test]
    fn keeps_internal_dashes_underscores_and_paths() {
        let line = "src/foo_bar-baz.rs";
        assert_eq!(word_at_col(line, 0).as_deref(), Some("src/foo_bar-baz.rs"));
    }
}

/// Walk left and right from `target_col` in `line` (using display widths) to
/// find the run of non-whitespace characters covering that column.
fn word_at_col(line: &str, target_col: usize) -> Option<String> {
    use unicode_width::UnicodeWidthChar;
    let mut col = 0usize;
    let mut hit_byte: Option<usize> = None;
    for (i, ch) in line.char_indices() {
        let w = ch.width().unwrap_or(0);
        if target_col >= col && target_col < col + w.max(1) {
            hit_byte = Some(i);
            break;
        }
        col += w;
    }
    let hit = hit_byte?;
    let bytes = line.as_bytes();
    if bytes.get(hit).map(|b| (*b as char).is_whitespace()).unwrap_or(true) {
        return None;
    }
    let mut start = hit;
    while start > 0 {
        let prev = line[..start].chars().next_back()?;
        if prev.is_whitespace() { break; }
        start -= prev.len_utf8();
    }
    let mut end = hit;
    let mut iter = line[hit..].char_indices();
    iter.next(); // skip the hit char itself
    let len = line.len();
    let mut cursor = hit;
    for (_, ch) in line[hit..].char_indices() {
        if ch.is_whitespace() { break; }
        cursor += ch.len_utf8();
        end = cursor;
    }
    let _ = iter;
    let _ = len;
    if end <= start { return None; }
    let word = line[start..end].trim_matches(|c: char| {
        // Strip leading/trailing punctuation but keep internal characters.
        c.is_ascii_punctuation() && !matches!(c, '_' | '-' | '/' | '.' | '#')
    });
    if word.is_empty() { None } else { Some(word.to_string()) }
}

fn point_in(rect: ratatui::layout::Rect, col: u16, row: u16) -> bool {
    col >= rect.x && col < rect.x + rect.width && row >= rect.y && row < rect.y + rect.height
}

/// Mouse-wheel scroll with rate-aware dampening: deliberate single ticks
/// (>200ms apart) pass through at full strength; rapid bursts get scaled
/// down to 0.5× so trackpad momentum scrolls don't fly off the page. The
/// fractional accumulator on `App` carries sub-line credit across events
/// so the user sees smooth movement instead of stutter.
fn wheel_scroll(app: &mut App, raw_delta: i32) {
    let now = std::time::Instant::now();
    let dampened = compute_dampened_scroll(
        &mut app.last_scroll_at,
        &mut app.scroll_accum,
        raw_delta,
        now,
    );
    if dampened != 0 {
        scroll_by(app, dampened);
    }
}

/// Pure dampening logic, factored out for tests. The `now` parameter lets
/// callers feed deterministic timestamps. Updates `last_at` and `accum`
/// in place; returns the integer-line delta to apply to scroll position.
fn compute_dampened_scroll(
    last_at: &mut Option<std::time::Instant>,
    accum: &mut f32,
    requested: i32,
    now: std::time::Instant,
) -> i32 {
    let elapsed_ms = last_at
        .map(|t| now.duration_since(t).as_millis() as u32)
        .unwrap_or(u32::MAX);
    // Long pause → drop fractional credit so a scroll started minutes ago
    // doesn't suddenly move an extra line on the next event.
    if elapsed_ms > 500 {
        *accum = 0.0;
    }
    // Reverse direction → reset, otherwise leftover credit in one direction
    // would silently absorb the first line of the opposite scroll.
    if (*accum > 0.0 && requested < 0) || (*accum < 0.0 && requested > 0) {
        *accum = 0.0;
    }
    // Linear ramp: ≤100ms apart → 0.5× (burst), ≥200ms → 1.0× (deliberate).
    let factor = ((elapsed_ms as f32) / 200.0).clamp(0.5, 1.0);
    *accum += (requested as f32) * factor;
    let lines = accum.trunc() as i32;
    *accum -= lines as f32;
    *last_at = Some(now);
    lines
}

fn scroll_by(app: &mut App, delta: i32) {
    match &mut app.view {
        View::Reader(r) => {
            let total = r.rendered.as_ref().map(|x| x.lines.len()).unwrap_or(0) as i32;
            let h = app.viewport.height as i32;
            let max = (total - h).max(0);
            let new = (r.scroll as i32 + delta).clamp(0, max) as u16;
            r.scroll = new;
            app.status.clear();
        }
        View::Browser(b) => {
            let n = b.entries.len() as i32;
            if n == 0 { return; }
            let new = (b.selected as i32 + delta).clamp(0, n - 1) as usize;
            b.selected = new;
            // Keep selection visible. Account for the bordered title row.
            let h = app.viewport.height.saturating_sub(2) as usize;
            if b.selected < b.scroll as usize {
                b.scroll = b.selected as u16;
            } else if b.selected >= b.scroll as usize + h.max(1) {
                b.scroll = (b.selected + 1 - h.max(1)) as u16;
            }
        }
    }
}

fn scroll_by_page(app: &mut App, dir: i32, half: bool) {
    let h = app.viewport.height as i32;
    let amt = if half { (h / 2).max(1) } else { (h - 1).max(1) };
    scroll_by(app, dir * amt);
}

fn scroll_to(app: &mut App, line: u16) {
    if let View::Reader(r) = &mut app.view {
        let total = r.rendered.as_ref().map(|x| x.lines.len()).unwrap_or(0) as i32;
        let h = app.viewport.height as i32;
        let max = (total - h).max(0) as u16;
        r.scroll = line.min(max);
    }
}

/// Right-arrow / Enter on a Browser: navigate into a directory (replacing
/// the current view, pushing it onto history) or open a file. No-op
/// outside Browser.
fn enter_or_open(app: &mut App) -> Result<()> {
    let entry = match &app.view {
        View::Browser(b) => b.entries.get(b.selected).cloned(),
        _ => return Ok(()),
    };
    if let Some(entry) = entry {
        activate_browser_entry(app, entry)?;
    }
    Ok(())
}

fn focus_next(app: &mut App) {
    walk_focus(app, 1);
}

fn focus_prev(app: &mut App) {
    walk_focus(app, -1);
}

/// Cycle focus by `delta` through the unified link+checkbox list.
fn walk_focus(app: &mut App, delta: i32) {
    if let View::Reader(r) = &mut app.view {
        let targets = r.focus_targets();
        let n = targets.len();
        if n == 0 { return; }
        let cur_idx = r.focus.and_then(|f| targets.iter().position(|(t, _, _)| *t == f));
        let new_idx: usize = match cur_idx {
            Some(i) => ((i as i32 + delta).rem_euclid(n as i32)) as usize,
            None if delta >= 0 => 0,
            None => n - 1,
        };
        let (focus, line, _col) = targets[new_idx];
        r.focus = Some(focus);
        center_on_line(app, line);
    }
}

fn center_on_line(app: &mut App, line: usize) {
    let h = app.viewport.height as usize;
    if let View::Reader(r) = &mut app.view {
        let scroll = r.scroll as usize;
        if line < scroll || line >= scroll + h.saturating_sub(1) {
            let new = line.saturating_sub(h / 2);
            r.scroll = new as u16;
        }
    }
}

fn activate(app: &mut App) -> Result<()> {
    let focus = match &app.view {
        View::Reader(r) => r.focus,
        _ => None,
    };
    match focus {
        Some(Focus::Link(fi)) => {
            if let View::Reader(r) = &app.view {
                if let Some(rendered) = &r.rendered {
                    if let Some(link) = rendered.link_map.links.get(fi) {
                        let target = link.target.clone();
                        app.follow(target)?;
                    }
                }
            }
            return Ok(());
        }
        Some(Focus::Checkbox(ci)) => {
            app.toggle_checkbox(ci)?;
            return Ok(());
        }
        None => {}
    }
    if let View::Browser(b) = &app.view {
        let entry = b.entries.get(b.selected).cloned();
        if let Some(e) = entry {
            activate_browser_entry(app, e)?;
        }
    }
    Ok(())
}

fn activate_browser_entry(app: &mut App, entry: BrowserEntry) -> Result<()> {
    match entry.kind {
        // Parent / dir: re-root the browser one level. Reader files: open.
        BrowserEntryKind::ParentDir | BrowserEntryKind::Dir => {
            app.navigate_to(EntryKind::Directory(entry.path), 0)?;
        }
        BrowserEntryKind::Markdown => {
            app.navigate_to(EntryKind::File(entry.path), 0)?;
        }
    }
    Ok(())
}

fn open_focused(app: &mut App) -> Result<()> {
    if let View::Reader(r) = &app.view {
        if let Some(Focus::Link(fi)) = r.focus {
            if let Some(rendered) = &r.rendered {
                if let Some(link) = rendered.link_map.links.get(fi) {
                    if let crate::links::LinkTarget::Url(u) = &link.target {
                        let _ = open::that_detached(u);
                        app.status = format!("Opened {}", u);
                    }
                }
            }
        }
    }
    Ok(())
}

fn update_hover(app: &mut App, col: u16, row: u16) {
    let area = app.viewport;
    if let View::Reader(r) = &mut app.view {
        let Some(rendered) = &r.rendered else { return; };
        let line_num_w = if app.opts.line_numbers {
            (format!("{}", rendered.lines.len()).len() + 1) as u16
        } else { 0 };
        let inner_x = area.x + line_num_w;
        if col < inner_x {
            r.hover_link = None;
            r.hover_checkbox = None;
            return;
        }
        let local_col = (col - inner_x) as usize;
        let local_row = (row - area.y) as usize;
        let line_idx = r.scroll as usize + local_row;
        r.hover_link = rendered.link_map.at(line_idx, local_col);
        r.hover_checkbox = rendered.checkbox_map.at(line_idx, local_col);
    }
}

fn click_at(app: &mut App, col: u16, row: u16) -> Result<()> {
    let area = app.viewport;
    let entry_to_open = match &mut app.view {
        View::Reader(r) => {
            let Some(rendered) = &r.rendered else { return Ok(()); };
            let line_num_w = if app.opts.line_numbers {
                (format!("{}", rendered.lines.len()).len() + 1) as u16
            } else { 0 };
            let inner_x = area.x + line_num_w;
            if col < inner_x { return Ok(()); }
            let local_col = (col - inner_x) as usize;
            let local_row = (row - area.y) as usize;
            let line_idx = r.scroll as usize + local_row;
            // Checkbox takes priority over link (the marker isn't part of any link).
            if let Some(ci) = rendered.checkbox_map.at(line_idx, local_col) {
                app.toggle_checkbox(ci)?;
                return Ok(());
            }
            if let Some(li) = rendered.link_map.at(line_idx, local_col) {
                let target = rendered.link_map.links[li].target.clone();
                r.focus = Some(Focus::Link(li));
                app.follow(target)?;
            }
            None
        }
        View::Browser(b) => {
            let local_row = (row - area.y) as usize;
            // Row 0 is the bordered title; list rows start at 1.
            let visual = local_row.saturating_sub(1);
            let idx = visual + b.scroll as usize;
            if idx < b.entries.len() {
                b.selected = idx;
                Some(b.entries[idx].clone())
            } else {
                None
            }
        }
    };
    if let Some(entry) = entry_to_open {
        activate_browser_entry(app, entry)?;
    }
    Ok(())
}
