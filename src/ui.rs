use std::io::{self, Stdout};

use anyhow::Result;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Frame;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};

use crate::app::{self, App, BrowserEntryKind, View};
use crate::links::LinkTarget;

pub type Term = Terminal<CrosstermBackend<Stdout>>;

pub fn setup_terminal() -> Result<Term> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let term = Terminal::new(backend)?;
    Ok(term)
}

pub fn restore_terminal(term: &mut Term) -> Result<()> {
    disable_raw_mode()?;
    execute!(
        term.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    term.show_cursor()?;
    Ok(())
}

/// Restore raw/alt screen state for use in a panic hook (best-effort).
pub fn restore_raw() -> Result<()> {
    let _ = disable_raw_mode();
    let mut out = io::stdout();
    let _ = execute!(out, LeaveAlternateScreen, DisableMouseCapture);
    Ok(())
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header
            Constraint::Min(1),    // body
            Constraint::Length(1), // status
        ])
        .split(area);

    let header = chunks[0];
    let body = chunks[1];
    let status = chunks[2];
    app.viewport = body;
    app.header_area = header;

    // Reserve 1 column on the right for the reader scrollbar so layout stays
    // stable whether or not content overflows. Browser ignores this width.
    app.ensure_rendered(body.width.saturating_sub(1));

    draw_header(f, app, header);
    match &app.view {
        View::Reader(_) => draw_reader(f, app, body),
        View::Browser(_) => draw_browser(f, app, body),
    }
    if matches!(app.view, View::Reader(_)) {
        draw_images_overlay(f, app, body);
    }

    draw_status(f, app, status);

    if app.search.is_some() {
        draw_search(f, app, area);
    }

    if app.help_open {
        draw_help(f, area);
    }
}

fn draw_header(f: &mut Frame, app: &mut App, area: Rect) {
    if area.width == 0 || area.height == 0 { return; }
    let theme = &app.opts.theme;

    let path = match &app.view {
        View::Reader(r) => match &r.origin {
            crate::app::ReaderOrigin::File(p) => display_path(p, &app.root),
            crate::app::ReaderOrigin::Stdin => "<stdin>".to_string(),
        },
        View::Browser(b) => format!("{}/", display_path(&b.dir, &app.root)),
    };

    let mut spans: Vec<Span> = Vec::new();
    app.back_button_hit = None;
    if !app.history.is_empty() {
        let label = " ‹ Back ";
        let start_x = area.x;
        let end_x = area.x + label.chars().count() as u16;
        spans.push(Span::styled(
            label.to_string(),
            Style::default()
                .bg(theme.status_bg)
                .fg(theme.status_fg)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw(" "));
        app.back_button_hit = Some((start_x, end_x));
    }
    spans.push(Span::styled(path, Style::default().add_modifier(Modifier::BOLD)));

    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Render `p` shortened against the launch root (or `~/`) when one is a prefix.
fn display_path(p: &std::path::Path, root: &std::path::Path) -> String {
    if let Ok(rel) = p.strip_prefix(root) {
        let s = rel.display().to_string();
        if s.is_empty() { return ".".to_string(); }
        return s;
    }
    if let Some(home) = dirs::home_dir() {
        if let Ok(rel) = p.strip_prefix(&home) {
            return format!("~/{}", rel.display());
        }
    }
    p.display().to_string()
}

fn draw_reader(f: &mut Frame, app: &mut App, area: Rect) {
    let View::Reader(r) = &app.view else { return; };
    let Some(rendered) = &r.rendered else { return; };
    let theme = &app.opts.theme;

    let total = rendered.lines.len();
    let scroll = (r.scroll as usize).min(total.saturating_sub(1));
    let visible_h = area.height as usize;

    let line_num_w = if app.opts.line_numbers {
        format!("{}", total).len() as u16 + 1
    } else { 0 };
    let scrollbar_w: u16 = 1;
    let line_num_area = Rect { x: area.x, y: area.y, width: line_num_w, height: area.height };
    let body_area = Rect {
        x: area.x + line_num_w,
        y: area.y,
        width: area.width.saturating_sub(line_num_w).saturating_sub(scrollbar_w),
        height: area.height,
    };
    let scrollbar_area = Rect {
        x: area.x + area.width.saturating_sub(scrollbar_w),
        y: area.y,
        width: scrollbar_w,
        height: area.height,
    };

    let mut display_lines: Vec<Line> = Vec::with_capacity(visible_h);
    let mut nums: Vec<Line> = Vec::with_capacity(visible_h);
    for i in 0..visible_h {
        let idx = scroll + i;
        if idx >= total { break; }
        let mut line = rendered.lines[idx].clone();
        if let Some(fi) = r.focused_link {
            if let Some(link) = rendered.link_map.links.get(fi) {
                if link.line == idx {
                    highlight_focused(&mut line, link, theme);
                }
            }
        }
        if let Some(hi) = r.hover_link {
            if let Some(link) = rendered.link_map.links.get(hi) {
                if link.line == idx {
                    highlight_focused(&mut line, link, theme);
                }
            }
        }
        if let Some(ci) = r.hover_checkbox {
            if let Some(cb) = rendered.checkbox_map.items.get(ci) {
                if cb.line == idx {
                    highlight_checkbox_hover(&mut line, cb.col_start, cb.col_end);
                }
            }
        }
        if let Some(s) = r.doc_search.as_ref() {
            for (mi, m) in s.matches.iter().enumerate() {
                if m.line == idx {
                    let is_current = !s.editing && mi == s.current;
                    highlight_doc_match(&mut line, m.col_start, m.col_end, is_current, theme);
                }
            }
        }
        display_lines.push(line);
        if app.opts.line_numbers {
            nums.push(Line::from(Span::styled(
                format!("{:>width$} ", idx + 1, width = (line_num_w as usize).saturating_sub(1)),
                Style::default().fg(theme.muted),
            )));
        }
    }

    if app.opts.line_numbers {
        f.render_widget(Paragraph::new(nums), line_num_area);
    }
    f.render_widget(Paragraph::new(display_lines), body_area);
    draw_scrollbar(f, scrollbar_area, scroll, total, visible_h, theme);
}

/// Render any visible images in the document on top of the body, using the
/// terminal's detected graphics protocol. No-op if the picker isn't available
/// or if the image fails to decode.
fn draw_images_overlay(f: &mut Frame, app: &mut App, body: Rect) {
    use ratatui_image::StatefulImage;

    if app.image_picker.is_none() { return; }
    let (images, scroll) = match &app.view {
        View::Reader(r) => match &r.rendered {
            Some(rd) => (rd.images.clone(), r.scroll as i32),
            None => return,
        },
        _ => return,
    };

    for img in &images {
        let rel_y = img.line as i32 - scroll;
        if rel_y < 0 || rel_y as u16 >= body.height { continue; }
        let path = match std::fs::canonicalize(&img.source) {
            Ok(p) => p,
            Err(_) => continue,
        };
        if !app.image_protocols.contains_key(&path) {
            let dyn_img = match image::ImageReader::open(&path) {
                Ok(rdr) => match rdr.decode() {
                    Ok(d) => d,
                    Err(_) => continue,
                },
                Err(_) => continue,
            };
            let proto = match app.image_picker.as_ref() {
                Some(p) => p.new_resize_protocol(dyn_img),
                None => continue,
            };
            app.image_protocols.insert(path.clone(), proto);
        }
        let proto = match app.image_protocols.get_mut(&path) {
            Some(p) => p,
            None => continue,
        };
        let max_h = body.height.saturating_sub(rel_y as u16);
        let h = 12u16.min(max_h);
        if h == 0 { continue; }
        let area = Rect {
            x: body.x,
            y: body.y + rel_y as u16,
            width: body.width.saturating_sub(1),
            height: h,
        };
        f.render_stateful_widget(StatefulImage::default(), area, proto);
    }
}

/// Vertical scrollbar with a thumb sized proportionally to the visible viewport
/// (`thumb_h ≈ visible_h * track_h / total`). When content fits entirely the
/// thumb fills the track.
fn draw_scrollbar(
    f: &mut Frame,
    area: Rect,
    scroll: usize,
    total: usize,
    visible_h: usize,
    theme: &crate::theme::Theme,
) {
    let track_h = area.height as usize;
    if track_h == 0 || area.width == 0 { return; }

    let track_style = Style::default().fg(theme.muted);
    let thumb_style = Style::default().fg(theme.heading[0]);

    let (thumb_top, thumb_h) = if total <= visible_h || total == 0 {
        (0, track_h)
    } else {
        let h = ((track_h * visible_h) / total).max(1).min(track_h);
        let max_scroll = total - visible_h;
        let span = track_h - h;
        let top = if max_scroll == 0 { 0 } else {
            (scroll * span + max_scroll / 2) / max_scroll
        };
        (top.min(span), h)
    };

    let lines: Vec<Line> = (0..track_h)
        .map(|i| {
            let in_thumb = i >= thumb_top && i < thumb_top + thumb_h;
            if in_thumb {
                Line::from(Span::styled("█", thumb_style))
            } else {
                Line::from(Span::styled("│", track_style))
            }
        })
        .collect();
    f.render_widget(Paragraph::new(lines), area);
}

fn highlight_focused(
    line: &mut Line<'_>,
    link: &crate::links::LinkSpan,
    theme: &crate::theme::Theme,
) {
    let mut col = 0usize;
    for span in &mut line.spans {
        let w = unicode_width::UnicodeWidthStr::width(span.content.as_ref());
        let span_start = col;
        let span_end = col + w;
        if span_start >= link.col_start && span_end <= link.col_end {
            span.style = span
                .style
                .fg(theme.link_focused)
                .add_modifier(Modifier::REVERSED);
        }
        col = span_end;
    }
}

/// Paint a match span. Non-current matches get the code-block background
/// (subtle); the current match gets the link-focus color reversed.
fn highlight_doc_match(
    line: &mut Line<'_>,
    col_start: usize,
    col_end: usize,
    is_current: bool,
    theme: &crate::theme::Theme,
) {
    let mut col = 0usize;
    for span in &mut line.spans {
        let w = unicode_width::UnicodeWidthStr::width(span.content.as_ref());
        let span_start = col;
        let span_end = col + w;
        if span_start >= col_start && span_end <= col_end {
            span.style = if is_current {
                span.style
                    .fg(theme.link_focused)
                    .add_modifier(Modifier::REVERSED)
                    .add_modifier(Modifier::BOLD)
            } else {
                span.style.add_modifier(Modifier::REVERSED)
            };
        }
        col = span_end;
    }
}

/// Paint reverse-video over spans that fall within `[col_start, col_end)` to
/// signal a hovered checkbox marker.
fn highlight_checkbox_hover(line: &mut Line<'_>, col_start: usize, col_end: usize) {
    let mut col = 0usize;
    for span in &mut line.spans {
        let w = unicode_width::UnicodeWidthStr::width(span.content.as_ref());
        let span_start = col;
        let span_end = col + w;
        if span_start >= col_start && span_end <= col_end {
            span.style = span.style.add_modifier(Modifier::REVERSED);
        }
        col = span_end;
    }
}

fn draw_browser(f: &mut Frame, app: &App, area: Rect) {
    let View::Browser(b) = &app.view else { return; };
    let theme = &app.opts.theme;
    let title = format!(" {} ", b.dir.display());
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(theme.muted));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let items: Vec<ListItem> = b
        .entries
        .iter()
        .map(|e| {
            let indent = "  ".repeat(e.depth);
            let prefix = match e.kind {
                BrowserEntryKind::Dir => {
                    if b.expanded.contains(&e.path) { "▾ " } else { "▸ " }
                }
                BrowserEntryKind::Markdown => "  ",
                BrowserEntryKind::ParentDir => "",
            };
            let label = format!("{}{}{}", indent, prefix, e.display);
            ListItem::new(Span::styled(label, browser_entry_style(e.kind, theme)))
        })
        .collect();
    let list = List::new(items)
        .highlight_style(
            Style::default()
                .bg(theme.status_bg)
                .fg(theme.status_fg)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");
    let mut state = ListState::default()
        .with_offset(b.scroll as usize)
        .with_selected(Some(b.selected));
    f.render_stateful_widget(list, inner, &mut state);
}

fn browser_entry_style(kind: BrowserEntryKind, theme: &crate::theme::Theme) -> Style {
    match kind {
        BrowserEntryKind::ParentDir => Style::default().fg(theme.muted),
        BrowserEntryKind::Dir => Style::default()
            .fg(theme.heading[0])
            .add_modifier(Modifier::BOLD),
        BrowserEntryKind::Markdown => Style::default(),
    }
}

fn draw_search(f: &mut Frame, app: &App, area: Rect) {
    let Some(s) = &app.search else { return; };
    let theme = &app.opts.theme;

    let w = area.width.saturating_sub(8).max(20).min(100);
    let h = area.height.saturating_sub(4).max(8);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let popup = Rect { x, y, width: w, height: h };

    f.render_widget(Clear, popup);

    let title = format!(" Search [{}] ", s.results.len());
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(theme.heading[0]));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(1)])
        .split(inner);

    let prompt = Line::from(vec![
        Span::styled(
            "▸ ",
            Style::default().fg(theme.heading[0]).add_modifier(Modifier::BOLD),
        ),
        Span::raw(s.query.clone()),
        Span::styled("█", Style::default().fg(theme.heading[0])),
        Span::styled(
            format!("   {}", short_root(&app.root)),
            Style::default().fg(theme.muted),
        ),
    ]);
    f.render_widget(Paragraph::new(prompt), layout[0]);

    let items: Vec<ListItem> = s
        .results
        .iter()
        .map(|r| {
            let style = if r.is_dir {
                Style::default()
                    .fg(theme.heading[0])
                    .add_modifier(Modifier::BOLD)
            } else if app::is_markdown_file(&r.path) {
                Style::default()
            } else {
                Style::default().fg(theme.muted)
            };
            let display = if r.is_dir {
                format!("{}/", r.display)
            } else {
                r.display.clone()
            };
            ListItem::new(Span::styled(display, style))
        })
        .collect();

    let list = List::new(items)
        .highlight_style(
            Style::default()
                .bg(theme.status_bg)
                .fg(theme.status_fg)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");
    let mut state = ListState::default().with_selected(Some(s.selected));
    f.render_stateful_widget(list, layout[1], &mut state);
}

fn short_root(p: &std::path::Path) -> String {
    if let Some(home) = dirs::home_dir() {
        if let Ok(rel) = p.strip_prefix(&home) {
            return format!("~/{}", rel.display());
        }
    }
    p.display().to_string()
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let theme = &app.opts.theme;

    let pos = match &app.view {
        View::Reader(r) => {
            let total = r.rendered.as_ref().map(|x| x.lines.len()).unwrap_or(0);
            let h = app.viewport.height as usize;
            if total == 0 || total <= h {
                "All".to_string()
            } else {
                let max_scroll = total - h;
                let pct = ((r.scroll as usize) * 100 / max_scroll).min(100);
                format!("{}%", pct)
            }
        }
        View::Browser(b) => format!("{}/{}", b.selected + 1, b.entries.len().max(1)),
    };

    let mut middle = String::new();
    if let View::Reader(r) = &app.view {
        if let (Some(rendered), Some(hi)) = (r.rendered.as_ref(), r.hover_link.or(r.focused_link)) {
            if let Some(link) = rendered.link_map.links.get(hi) {
                middle = describe_target(&link.target);
            }
        }
        if let Some(s) = r.doc_search.as_ref() {
            if s.editing {
                middle = format!("/{}_", s.query);
            } else if s.matches.is_empty() {
                middle = format!("no match: /{}", s.query);
            } else {
                middle = format!(
                    "/{}  [{}/{}]",
                    s.query,
                    s.current + 1,
                    s.matches.len()
                );
            }
        }
    }
    if !app.status.is_empty() {
        middle = app.status.clone();
    }

    let mid = Span::styled(format!(" {} ", middle), Style::default().fg(theme.muted));
    let right = Span::styled(
        format!(" {} ", pos),
        Style::default()
            .bg(theme.status_bg)
            .fg(theme.status_fg)
            .add_modifier(Modifier::BOLD),
    );

    let used = unicode_width::UnicodeWidthStr::width(mid.content.as_ref())
        + unicode_width::UnicodeWidthStr::width(right.content.as_ref());
    let pad = (area.width as usize).saturating_sub(used);
    let line = Line::from(vec![mid, Span::raw(" ".repeat(pad)), right]);
    f.render_widget(Paragraph::new(line), area);
}

fn describe_target(t: &LinkTarget) -> String {
    match t {
        LinkTarget::Url(u) => u.clone(),
        LinkTarget::LocalFile(p) => p.display().to_string(),
        LinkTarget::Anchor(a) => format!("#{}", a),
        LinkTarget::FileAnchor(p, a) => format!("{}#{}", p.display(), a),
    }
}

fn draw_help(f: &mut Frame, area: Rect) {
    let w = 60.min(area.width.saturating_sub(4));
    let h = 30.min(area.height.saturating_sub(4));
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let popup = Rect { x, y, width: w, height: h };
    f.render_widget(Clear, popup);
    let body = vec![
        Line::from("md keybindings"),
        Line::from(""),
        Line::from("  j / ↓        scroll down / next entry"),
        Line::from("  k / ↑        scroll up / prev entry"),
        Line::from("  d / PgDn     half/page down"),
        Line::from("  u / PgUp     half/page up"),
        Line::from("  g / G        top / bottom"),
        Line::from("  Tab / S-Tab  next / prev link"),
        Line::from("  Enter        open file / toggle dir expansion / link"),
        Line::from("  → / ←        expand-or-open / collapse-or-parent"),
        Line::from("  /            in-doc text search (Reader) / file search (Browser)"),
        Line::from("  n / N        next / prev match"),
        Line::from("  T            fuzzy file search (anywhere)"),
        Line::from("  h / b        back   (history)"),
        Line::from("  l / f        forward (history)"),
        Line::from("  e            edit current file in $EDITOR"),
        Line::from("  o            open in browser (focused link)"),
        Line::from("  m            toggle mouse (drag-to-select)"),
        Line::from("  q / Esc      quit"),
        Line::from("  ?            toggle this help"),
        Line::from(""),
        Line::from("  In search:   type to filter, ↑/↓ navigate,"),
        Line::from("               Enter open, Esc cancel,"),
        Line::from("               Ctrl-U clear query."),
        Line::from(""),
        Line::from("  Mouse: wheel scrolls; click follows links."),
    ];
    let block = Block::default().borders(Borders::ALL).title(" Help ");
    let para = Paragraph::new(body).block(block);
    f.render_widget(para, popup);
}
