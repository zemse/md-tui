use std::time::Duration;

use anyhow::Result;
use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};

use crate::app::{self, App, BrowserEntry, BrowserEntryKind, EntryKind, SearchResult, View};
use crate::ui;

pub fn run(term: &mut ui::Term, app: &mut App) -> Result<()> {
    while !app.should_quit {
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
    match key.code {
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Esc => {
            if let View::Reader(_) = app.view {
                if !app.history.is_empty() {
                    app.go_back()?;
                } else {
                    app.should_quit = true;
                }
            } else {
                app.should_quit = true;
            }
        }
        KeyCode::Char('?') => app.help_open = !app.help_open,
        KeyCode::Char('/') => app.open_search(),
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.should_quit = true;
        }

        // Navigation
        KeyCode::Char('h') | KeyCode::Char('b') | KeyCode::Backspace => app.go_back()?,
        KeyCode::Char('l') | KeyCode::Char('f') => app.go_forward()?,

        // Scrolling
        KeyCode::Char('j') | KeyCode::Down => scroll_by(app, 1),
        KeyCode::Char('k') | KeyCode::Up => scroll_by(app, -1),
        KeyCode::Char('d') => scroll_by_page(app, 1, true),
        KeyCode::Char('u') => scroll_by_page(app, -1, true),
        KeyCode::PageDown | KeyCode::Char(' ') => scroll_by_page(app, 1, false),
        KeyCode::PageUp => scroll_by_page(app, -1, false),
        KeyCode::Char('g') | KeyCode::Home => scroll_to(app, 0),
        KeyCode::Char('G') | KeyCode::End => scroll_to(app, u16::MAX),

        // Link navigation
        KeyCode::Tab => focus_next_link(app),
        KeyCode::BackTab => focus_prev_link(app),
        KeyCode::Enter => activate(app)?,
        KeyCode::Char('o') => open_focused(app)?,

        _ => {}
    }
    Ok(())
}

fn handle_search_key(app: &mut App, key: KeyEvent) -> Result<()> {
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
            MouseEventKind::ScrollUp => scroll_by(app, -3),
            MouseEventKind::ScrollDown => scroll_by(app, 3),
            _ => {}
        }
        return Ok(());
    }
    match m.kind {
        MouseEventKind::ScrollUp => scroll_by(app, -3),
        MouseEventKind::ScrollDown => scroll_by(app, 3),
        MouseEventKind::Moved => {
            update_hover(app, m.column, m.row);
        }
        MouseEventKind::Down(MouseButton::Left) => {
            click_at(app, m.column, m.row)?;
        }
        _ => {}
    }
    Ok(())
}

fn point_in(rect: ratatui::layout::Rect, col: u16, row: u16) -> bool {
    col >= rect.x && col < rect.x + rect.width && row >= rect.y && row < rect.y + rect.height
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

fn focus_next_link(app: &mut App) {
    if let View::Reader(r) = &mut app.view {
        let Some(rendered) = &r.rendered else { return; };
        let n = rendered.link_map.links.len();
        if n == 0 { return; }
        let next = match r.focused_link {
            Some(i) => (i + 1) % n,
            None => 0,
        };
        r.focused_link = Some(next);
        let line = rendered.link_map.links[next].line;
        center_on_line(app, line);
    }
}

fn focus_prev_link(app: &mut App) {
    if let View::Reader(r) = &mut app.view {
        let Some(rendered) = &r.rendered else { return; };
        let n = rendered.link_map.links.len();
        if n == 0 { return; }
        let prev = match r.focused_link {
            Some(0) | None => n - 1,
            Some(i) => i - 1,
        };
        r.focused_link = Some(prev);
        let line = rendered.link_map.links[prev].line;
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
    match &app.view {
        View::Reader(r) => {
            if let Some(fi) = r.focused_link {
                if let Some(rendered) = &r.rendered {
                    if let Some(link) = rendered.link_map.links.get(fi) {
                        let target = link.target.clone();
                        app.follow(target)?;
                    }
                }
            }
            Ok(())
        }
        View::Browser(b) => {
            let entry = b.entries.get(b.selected).cloned();
            if let Some(e) = entry {
                activate_browser_entry(app, e)?;
            }
            Ok(())
        }
    }
}

fn activate_browser_entry(app: &mut App, entry: BrowserEntry) -> Result<()> {
    match entry.kind {
        BrowserEntryKind::ParentDir | BrowserEntryKind::Dir => {
            app.navigate_to(EntryKind::Directory(entry.path), 0)?;
        }
        BrowserEntryKind::Markdown => {
            app.navigate_to(EntryKind::File(entry.path), 0)?;
        }
        BrowserEntryKind::Other => {
            let _ = open::that_detached(&entry.path);
            app.status = format!("Opened externally: {}", entry.path.display());
        }
    }
    Ok(())
}

fn open_focused(app: &mut App) -> Result<()> {
    if let View::Reader(r) = &app.view {
        if let Some(fi) = r.focused_link {
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
        if col < inner_x { r.hover_link = None; return; }
        let local_col = (col - inner_x) as usize;
        let local_row = (row - area.y) as usize;
        let line_idx = r.scroll as usize + local_row;
        r.hover_link = rendered.link_map.at(line_idx, local_col);
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
            if let Some(li) = rendered.link_map.at(line_idx, local_col) {
                let target = rendered.link_map.links[li].target.clone();
                r.focused_link = Some(li);
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
