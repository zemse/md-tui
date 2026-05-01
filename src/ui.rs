use std::io::{self, Stdout};

use anyhow::Result;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::app::{App, View};
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
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);

    let body = chunks[0];
    let status = chunks[1];
    app.viewport = body;

    app.ensure_rendered(body.width);

    match &app.view {
        View::Reader(_) => draw_reader(f, app, body),
        View::Browser(_) => draw_browser(f, app, body),
    }

    draw_status(f, app, status);

    if app.help_open {
        draw_help(f, area);
    }
}

fn draw_reader(f: &mut Frame, app: &App, area: Rect) {
    let View::Reader(r) = &app.view else { return; };
    let Some(rendered) = &r.rendered else { return; };
    let theme = &app.opts.theme;

    let total = rendered.lines.len();
    let scroll = (r.scroll as usize).min(total.saturating_sub(1));
    let visible_h = area.height as usize;

    let line_num_w = if app.opts.line_numbers {
        let w = format!("{}", total).len() as u16 + 1;
        w
    } else { 0 };
    let line_num_area = Rect { x: area.x, y: area.y, width: line_num_w, height: area.height };
    let body_area = Rect { x: area.x + line_num_w, y: area.y, width: area.width.saturating_sub(line_num_w), height: area.height };

    let mut display_lines: Vec<Line> = Vec::with_capacity(visible_h);
    let mut nums: Vec<Line> = Vec::with_capacity(visible_h);
    for i in 0..visible_h {
        let idx = scroll + i;
        if idx >= total { break; }
        let mut line = rendered.lines[idx].clone();
        // Highlight focused link.
        if let Some(fi) = r.focused_link {
            if let Some(link) = rendered.link_map.links.get(fi) {
                if link.line == idx {
                    highlight_focused(&mut line, link, theme);
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
}

fn highlight_focused(
    line: &mut Line<'_>,
    link: &crate::links::LinkSpan,
    theme: &crate::theme::Theme,
) {
    // Re-style spans whose horizontal position falls inside [col_start, col_end).
    // This is a best-effort post-pass; it relies on spans being rendered left-to-right.
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

fn draw_browser(f: &mut Frame, app: &App, area: Rect) {
    let View::Browser(b) = &app.view else { return; };
    let theme = &app.opts.theme;
    let title = format!(" {} ", b.root.display());
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(theme.muted));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let items: Vec<ListItem> = b
        .entries
        .iter()
        .map(|e| ListItem::new(e.display.clone()))
        .collect();
    let list = List::new(items)
        .highlight_style(
            Style::default()
                .bg(theme.status_bg)
                .fg(theme.status_fg)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");
    let mut state = ListState::default();
    state.select(Some(b.selected));
    f.render_stateful_widget(list, inner, &mut state);
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let theme = &app.opts.theme;
    let path = match &app.view {
        View::Reader(r) => match &r.origin {
            crate::app::ReaderOrigin::File(p) => p.display().to_string(),
            crate::app::ReaderOrigin::Stdin => "<stdin>".to_string(),
        },
        View::Browser(b) => b.root.display().to_string(),
    };

    let pos = match &app.view {
        View::Reader(r) => {
            let total = r.rendered.as_ref().map(|x| x.lines.len()).unwrap_or(0);
            let h = area.height.max(1) as usize;
            let _ = h;
            if total == 0 {
                "0%".to_string()
            } else {
                let pct = (r.scroll as usize * 100 / total.max(1)).min(100);
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
    }
    if !app.status.is_empty() {
        middle = app.status.clone();
    }

    let left = Span::styled(
        format!(" {} ", path),
        Style::default().bg(theme.status_bg).fg(theme.status_fg).add_modifier(Modifier::BOLD),
    );
    let mid = Span::styled(
        format!(" {} ", middle),
        Style::default().fg(theme.muted),
    );
    let right = Span::styled(
        format!(" {} ", pos),
        Style::default().bg(theme.status_bg).fg(theme.status_fg).add_modifier(Modifier::BOLD),
    );

    let used = unicode_width::UnicodeWidthStr::width(left.content.as_ref())
        + unicode_width::UnicodeWidthStr::width(mid.content.as_ref())
        + unicode_width::UnicodeWidthStr::width(right.content.as_ref());
    let pad = (area.width as usize).saturating_sub(used);
    let line = Line::from(vec![left, mid, Span::raw(" ".repeat(pad)), right]);
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
    let w = 56.min(area.width.saturating_sub(4));
    let h = 22.min(area.height.saturating_sub(4));
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let popup = Rect { x, y, width: w, height: h };
    f.render_widget(Clear, popup);
    let body = vec![
        Line::from("md-tui keybindings"),
        Line::from(""),
        Line::from("  j / ↓        scroll down"),
        Line::from("  k / ↑        scroll up"),
        Line::from("  d / PgDn     half/page down"),
        Line::from("  u / PgUp     half/page up"),
        Line::from("  g / G        top / bottom"),
        Line::from("  Tab / S-Tab  next / prev link"),
        Line::from("  Enter        follow focused link"),
        Line::from("  h / b        back"),
        Line::from("  l / f        forward"),
        Line::from("  o            open in browser (focused)"),
        Line::from("  q / Esc      quit"),
        Line::from("  ?            toggle this help"),
        Line::from(""),
        Line::from("  Mouse: wheel scrolls; click follows links."),
    ];
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Help ");
    let para = Paragraph::new(body).block(block);
    f.render_widget(para, popup);
}
