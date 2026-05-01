//! Markdown → ratatui rendering.
//!
//! Walks `pulldown-cmark` events into a token list, then runs a layout pass
//! that produces wrapped `Line<'static>`s plus a parallel `LinkMap` recording
//! click targets in (line, col_start, col_end) space.

use std::path::{Path, PathBuf};

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::links::{self, LinkMap, LinkSpan, LinkTarget};
use crate::syntax;
use crate::theme::Theme;

#[derive(Clone, Debug)]
pub struct Rendered {
    pub lines: Vec<Line<'static>>,
    pub link_map: LinkMap,
    pub width: u16,
}

pub fn render(source: &str, base_dir: Option<&Path>, width: u16, theme: &Theme) -> Rendered {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TASKLISTS);
    opts.insert(Options::ENABLE_FOOTNOTES);
    opts.insert(Options::ENABLE_SMART_PUNCTUATION);

    let parser = Parser::new_ext(source, opts);
    let mut b = Builder::new(theme.clone(), width as usize, base_dir.map(|p| p.to_path_buf()));
    for ev in parser {
        b.event(ev);
    }
    b.finish()
}

#[derive(Clone, Debug)]
struct Run {
    text: String,
    style: Style,
    /// Index into `links` if this run is part of a hyperlink.
    link: Option<usize>,
}

#[derive(Clone, Debug)]
enum Block {
    /// Flowing paragraph — word-wrapped.
    Paragraph { runs: Vec<Run>, prefix: Vec<Run>, hanging: Vec<Run> },
    /// Heading — word-wrapped, anchor recorded.
    Heading { runs: Vec<Run>, anchor: String },
    /// Pre-formatted block — rendered line-by-line, not wrapped.
    Pre { lines: Vec<Vec<Run>>, prefix: Vec<Run> },
    /// Horizontal rule.
    Rule,
    /// Empty line.
    Blank,
}

struct Builder {
    theme: Theme,
    width: usize,
    base_dir: Option<PathBuf>,

    blocks: Vec<Block>,
    /// Pending links awaiting layout — index in this vec is referenced by `Run.link`.
    links: Vec<PendingLink>,

    // assembly state for current block
    cur_runs: Vec<Run>,
    cur_prefix: Vec<Run>,
    cur_hanging: Vec<Run>,
    style_stack: Vec<Style>,

    // structural state
    list_stack: Vec<ListFrame>,
    quote_depth: usize,
    in_heading: Option<HeadingLevel>,
    heading_buf: String,

    // code block state
    code_lang: Option<String>,
    code_content: String,
    in_code_block: bool,

    // link state
    open_link: Option<usize>,

    // table state (very basic)
    in_table: bool,
}

struct PendingLink {
    target: LinkTarget,
}

#[derive(Clone, Debug)]
struct ListFrame {
    ordered: Option<u64>,
}

impl Builder {
    fn new(theme: Theme, width: usize, base_dir: Option<PathBuf>) -> Self {
        let width = if width == 0 { 80 } else { width };
        Self {
            theme,
            width,
            base_dir,
            blocks: Vec::new(),
            links: Vec::new(),
            cur_runs: Vec::new(),
            cur_prefix: Vec::new(),
            cur_hanging: Vec::new(),
            style_stack: vec![Style::default()],
            list_stack: Vec::new(),
            quote_depth: 0,
            in_heading: None,
            heading_buf: String::new(),
            code_lang: None,
            code_content: String::new(),
            in_code_block: false,
            open_link: None,
            in_table: false,
        }
    }

    fn cur_style(&self) -> Style {
        *self.style_stack.last().unwrap()
    }

    fn push_style(&mut self, mods: impl FnOnce(Style) -> Style) {
        let s = mods(self.cur_style());
        self.style_stack.push(s);
    }

    fn pop_style(&mut self) {
        if self.style_stack.len() > 1 {
            self.style_stack.pop();
        }
    }

    fn quote_prefix(&self) -> Vec<Run> {
        if self.quote_depth == 0 { return Vec::new(); }
        let bar = "│ ".repeat(self.quote_depth);
        vec![Run {
            text: bar,
            style: Style::default().fg(self.theme.quote),
            link: None,
        }]
    }

    fn list_prefixes(&mut self) -> (Vec<Run>, Vec<Run>) {
        if self.list_stack.is_empty() { return (Vec::new(), Vec::new()); }
        let depth = self.list_stack.len();
        let indent = "  ".repeat(depth - 1);
        let frame = self.list_stack.last_mut().unwrap();
        let marker = match &mut frame.ordered {
            Some(n) => {
                let s = format!("{}. ", *n);
                *n += 1;
                s
            }
            None => "• ".to_string(),
        };
        let pad = " ".repeat(marker.chars().count());
        let style = Style::default().fg(self.theme.list_marker);
        let prefix = vec![
            Run { text: indent.clone(), style: Style::default(), link: None },
            Run { text: marker, style, link: None },
        ];
        let hanging = vec![Run {
            text: format!("{}{}", indent, pad),
            style: Style::default(),
            link: None,
        }];
        (prefix, hanging)
    }

    fn finish_paragraph(&mut self) {
        if self.cur_runs.is_empty() && self.cur_prefix.is_empty() {
            return;
        }
        let mut prefix = self.quote_prefix();
        prefix.extend(self.cur_prefix.drain(..));
        let mut hanging = self.quote_prefix();
        hanging.extend(self.cur_hanging.drain(..));
        let runs = std::mem::take(&mut self.cur_runs);
        self.blocks.push(Block::Paragraph { runs, prefix, hanging });
    }

    fn push_text(&mut self, text: &str) {
        if self.in_code_block {
            self.code_content.push_str(text);
            return;
        }
        if let Some(_) = self.in_heading {
            self.heading_buf.push_str(text);
        }
        let style = self.cur_style();
        self.cur_runs.push(Run {
            text: text.to_string(),
            style,
            link: self.open_link,
        });
    }

    fn event(&mut self, ev: Event<'_>) {
        match ev {
            Event::Start(tag) => self.start_tag(tag),
            Event::End(tag) => self.end_tag(tag),
            Event::Text(s) => self.push_text(&s),
            Event::Code(s) => {
                let style = self
                    .cur_style()
                    .fg(self.theme.code_fg)
                    .bg_opt(self.theme.code_bg);
                self.cur_runs.push(Run {
                    text: format!(" {} ", s),
                    style,
                    link: self.open_link,
                });
                if self.in_heading.is_some() {
                    self.heading_buf.push_str(&s);
                }
            }
            Event::Html(_) | Event::InlineHtml(_) => {}
            Event::FootnoteReference(name) => {
                let style = self.cur_style().fg(self.theme.muted);
                self.cur_runs.push(Run {
                    text: format!("[^{}]", name),
                    style,
                    link: self.open_link,
                });
            }
            Event::SoftBreak => {
                self.cur_runs.push(Run {
                    text: " ".to_string(),
                    style: self.cur_style(),
                    link: self.open_link,
                });
            }
            Event::HardBreak => {
                self.cur_runs.push(Run {
                    text: "\n".to_string(),
                    style: self.cur_style(),
                    link: self.open_link,
                });
            }
            Event::Rule => self.blocks.push(Block::Rule),
            Event::TaskListMarker(checked) => {
                let glyph = if checked { "[x] " } else { "[ ] " };
                let style = Style::default().fg(self.theme.list_marker).add_modifier(Modifier::BOLD);
                self.cur_runs.push(Run { text: glyph.to_string(), style, link: None });
            }
            _ => {}
        }
    }

    fn start_tag(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {}
            Tag::Heading { level, .. } => {
                self.in_heading = Some(level);
                self.heading_buf.clear();
                let lvl = heading_idx(level);
                let style = Style::default()
                    .fg(self.theme.heading[lvl])
                    .add_modifier(self.theme.heading_modifier);
                self.style_stack.push(style);
            }
            Tag::BlockQuote(_) => {
                self.quote_depth += 1;
            }
            Tag::CodeBlock(kind) => {
                self.in_code_block = true;
                self.code_content.clear();
                self.code_lang = match kind {
                    CodeBlockKind::Fenced(s) => {
                        let s = s.into_string();
                        if s.is_empty() { None } else { Some(s) }
                    }
                    CodeBlockKind::Indented => None,
                };
            }
            Tag::List(start) => {
                self.list_stack.push(ListFrame { ordered: start });
            }
            Tag::Item => {
                let (prefix, hanging) = self.list_prefixes();
                self.cur_prefix = prefix;
                self.cur_hanging = hanging;
            }
            Tag::Emphasis => {
                let m = self.theme.emphasis;
                self.push_style(|s| s.add_modifier(m));
            }
            Tag::Strong => {
                let m = self.theme.strong;
                self.push_style(|s| s.add_modifier(m));
            }
            Tag::Strikethrough => {
                let m = self.theme.strikethrough;
                self.push_style(|s| s.add_modifier(m));
            }
            Tag::Link { dest_url, .. } => {
                let target = links::resolve(&dest_url, self.base_dir.as_deref());
                let idx = self.links.len();
                self.links.push(PendingLink { target });
                self.open_link = Some(idx);
                let style = Style::default()
                    .fg(self.theme.link)
                    .add_modifier(self.theme.link_modifier);
                self.style_stack.push(style);
            }
            Tag::Image { dest_url, title, .. } => {
                let style = Style::default().fg(self.theme.muted);
                let label = if title.is_empty() {
                    format!("[image: {}]", dest_url)
                } else {
                    format!("[image: {} ({})]", title, dest_url)
                };
                self.cur_runs.push(Run { text: label, style, link: None });
            }
            Tag::Table(_) => { self.in_table = true; }
            Tag::TableHead | Tag::TableRow => {}
            Tag::TableCell => {}
            Tag::FootnoteDefinition(name) => {
                let style = Style::default().fg(self.theme.muted).add_modifier(Modifier::BOLD);
                self.cur_runs.push(Run {
                    text: format!("[^{}]: ", name),
                    style,
                    link: None,
                });
            }
            _ => {}
        }
    }

    fn end_tag(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                self.finish_paragraph();
                self.blocks.push(Block::Blank);
            }
            TagEnd::Heading(_) => {
                self.in_heading = None;
                let anchor = links::slugify(&self.heading_buf);
                self.style_stack.pop();
                let runs = std::mem::take(&mut self.cur_runs);
                self.blocks.push(Block::Blank);
                self.blocks.push(Block::Heading { runs, anchor });
                self.blocks.push(Block::Blank);
                self.cur_prefix.clear();
                self.cur_hanging.clear();
            }
            TagEnd::BlockQuote(_) => {
                if self.quote_depth > 0 { self.quote_depth -= 1; }
                if self.quote_depth == 0 {
                    self.blocks.push(Block::Blank);
                }
            }
            TagEnd::CodeBlock => {
                self.in_code_block = false;
                let mut highlighted: Vec<Vec<Span<'static>>> = Vec::new();
                let lang = self.code_lang.take();
                syntax::highlight(
                    &self.code_content,
                    lang.as_deref(),
                    self.theme.syntect_theme,
                    &mut highlighted,
                );
                let lines: Vec<Vec<Run>> = highlighted
                    .into_iter()
                    .map(|spans| {
                        spans
                            .into_iter()
                            .map(|sp| Run {
                                text: sp.content.into_owned(),
                                style: sp.style,
                                link: None,
                            })
                            .collect()
                    })
                    .collect();
                let prefix = self.quote_prefix();
                self.code_content.clear();
                self.blocks.push(Block::Pre { lines, prefix });
                self.blocks.push(Block::Blank);
            }
            TagEnd::List(_) => {
                self.list_stack.pop();
                if self.list_stack.is_empty() {
                    self.blocks.push(Block::Blank);
                }
            }
            TagEnd::Item => {
                self.finish_paragraph();
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => {
                self.pop_style();
            }
            TagEnd::Link => {
                self.style_stack.pop();
                self.open_link = None;
            }
            TagEnd::Table => {
                self.in_table = false;
                self.blocks.push(Block::Blank);
            }
            TagEnd::TableHead | TagEnd::TableRow => {
                self.finish_paragraph();
            }
            TagEnd::TableCell => {
                self.cur_runs.push(Run {
                    text: " │ ".to_string(),
                    style: Style::default().fg(self.theme.muted),
                    link: None,
                });
            }
            _ => {}
        }
    }

    fn finish(mut self) -> Rendered {
        self.finish_paragraph();
        layout(&self.theme, self.width, self.blocks, self.links)
    }
}

fn heading_idx(l: HeadingLevel) -> usize {
    match l {
        HeadingLevel::H1 => 0,
        HeadingLevel::H2 => 1,
        HeadingLevel::H3 => 2,
        HeadingLevel::H4 => 3,
        HeadingLevel::H5 => 4,
        HeadingLevel::H6 => 5,
    }
}

// ---------------------------------------------------------------------------
// Layout pass: blocks → wrapped lines + LinkMap
// ---------------------------------------------------------------------------

fn layout(theme: &Theme, width: usize, blocks: Vec<Block>, links: Vec<PendingLink>) -> Rendered {
    let mut out_lines: Vec<Line<'static>> = Vec::new();
    let mut out_links: Vec<LinkSpan> = Vec::new();
    let mut anchors = std::collections::HashMap::new();

    // For each link index, track the open span being built across runs.
    let mut open_spans: Vec<Option<OpenSpan>> = (0..links.len()).map(|_| None).collect();

    for block in blocks {
        match block {
            Block::Blank => {
                if out_lines.last().map(|l| l.spans.is_empty()).unwrap_or(false) {
                    continue;
                }
                out_lines.push(Line::from(""));
            }
            Block::Rule => {
                let bar = "─".repeat(width.max(1));
                out_lines.push(Line::from(Span::styled(
                    bar,
                    Style::default().fg(theme.rule),
                )));
            }
            Block::Heading { runs, anchor, .. } => {
                let start_line = out_lines.len();
                anchors.insert(anchor, start_line);
                let prefix = Vec::new();
                wrap_runs(
                    &runs,
                    &prefix,
                    &prefix,
                    width,
                    &mut out_lines,
                    &mut out_links,
                    &links,
                    &mut open_spans,
                );
            }
            Block::Paragraph { runs, prefix, hanging } => {
                wrap_runs(
                    &runs,
                    &prefix,
                    &hanging,
                    width,
                    &mut out_lines,
                    &mut out_links,
                    &links,
                    &mut open_spans,
                );
            }
            Block::Pre { lines, prefix } => {
                let pad_left = "  ";
                let prefix_width = prefix.iter().map(|r| r.text.width()).sum::<usize>() + pad_left.width();
                let bg = theme.code_bg;
                for line_runs in lines {
                    let mut spans: Vec<Span<'static>> = Vec::new();
                    for r in &prefix {
                        spans.push(Span::styled(r.text.clone(), r.style));
                    }
                    spans.push(Span::styled(
                        pad_left.to_string(),
                        Style::default().bg_opt(bg),
                    ));
                    let mut content_w = 0;
                    for r in line_runs {
                        let s = r.style.bg_opt(bg);
                        content_w += r.text.width();
                        spans.push(Span::styled(r.text, s));
                    }
                    let pad_right = width.saturating_sub(prefix_width + content_w);
                    if pad_right > 0 {
                        spans.push(Span::styled(
                            " ".repeat(pad_right),
                            Style::default().bg_opt(bg),
                        ));
                    }
                    out_lines.push(Line::from(spans));
                }
            }
        }
    }

    LinkMap::default(); // touch
    let link_map = LinkMap {
        links: out_links,
        anchors,
    };
    Rendered { lines: out_lines, link_map, width: width as u16 }
}

#[derive(Clone, Debug)]
struct OpenSpan {
    line: usize,
    col_start: usize,
    col_cur: usize,
}

/// Wrap `runs` to `width`, prepending `prefix` to the first physical line and
/// `hanging` to subsequent wrapped continuations. Pushes each physical line
/// to `out_lines`. Splits link spans across wrap boundaries by emitting one
/// `LinkSpan` per physical line that the link covers.
fn wrap_runs(
    runs: &[Run],
    prefix: &[Run],
    hanging: &[Run],
    width: usize,
    out_lines: &mut Vec<Line<'static>>,
    out_links: &mut Vec<LinkSpan>,
    links: &[PendingLink],
    open_spans: &mut [Option<OpenSpan>],
) {
    let prefix_width: usize = prefix.iter().map(|r| r.text.width()).sum();
    let hanging_width: usize = hanging.iter().map(|r| r.text.width()).sum();
    let inner_width = width.saturating_sub(prefix_width).max(1);
    let cont_inner_width = width.saturating_sub(hanging_width).max(1);

    let mut cur_spans: Vec<Span<'static>> = prefix
        .iter()
        .map(|r| Span::styled(r.text.clone(), r.style))
        .collect();
    let mut cur_col = prefix_width;
    let mut cur_inner = 0usize;
    let mut at_line_start = true;
    let mut active_inner_width = inner_width;

    let line_idx = |out: &Vec<Line<'static>>| out.len();
    let _ = line_idx;

    let mut current_link: Option<usize> = None;

    let push_run_chunk = |cur_spans: &mut Vec<Span<'static>>,
                          cur_col: &mut usize,
                          cur_inner: &mut usize,
                          run: &Run,
                          chunk: &str| {
        if chunk.is_empty() { return; }
        let w = chunk.width();
        cur_spans.push(Span::styled(chunk.to_string(), run.style));
        *cur_col += w;
        *cur_inner += w;
    };

    let break_line = |
        out_lines: &mut Vec<Line<'static>>,
        cur_spans: &mut Vec<Span<'static>>,
        cur_col: &mut usize,
        cur_inner: &mut usize,
        active_inner_width: &mut usize,
        at_line_start: &mut bool,
        out_links: &mut Vec<LinkSpan>,
        open_spans: &mut [Option<OpenSpan>],
        current_link: &mut Option<usize>,
        links: &[PendingLink],
        hanging: &[Run],
        cont_inner_width: usize,
    | {
        // Close any open link span on this line.
        if let Some(li) = *current_link {
            if let Some(open) = open_spans[li].take() {
                let line = out_lines.len();
                if open.line == line && open.col_cur > open.col_start {
                    out_links.push(LinkSpan {
                        line: open.line,
                        col_start: open.col_start,
                        col_end: open.col_cur,
                        target: links[li].target.clone(),
                    });
                }
            }
        }
        out_lines.push(Line::from(std::mem::take(cur_spans)));
        // Start a new line with hanging prefix.
        for r in hanging {
            cur_spans.push(Span::styled(r.text.clone(), r.style));
        }
        *cur_col = hanging.iter().map(|r| r.text.width()).sum();
        *cur_inner = 0;
        *active_inner_width = cont_inner_width;
        *at_line_start = true;
        // Reopen link span at new line if a link is still in progress.
        if let Some(li) = *current_link {
            open_spans[li] = Some(OpenSpan {
                line: out_lines.len(),
                col_start: *cur_col,
                col_cur: *cur_col,
            });
        }
    };

    for run in runs {
        // Switch link tracking if needed.
        if run.link != current_link {
            // Close old.
            if let Some(li) = current_link {
                if let Some(open) = open_spans[li].take() {
                    let line = out_lines.len();
                    if open.line == line && open.col_cur > open.col_start {
                        out_links.push(LinkSpan {
                            line: open.line,
                            col_start: open.col_start,
                            col_end: open.col_cur,
                            target: links[li].target.clone(),
                        });
                    }
                }
            }
            current_link = run.link;
            if let Some(li) = current_link {
                open_spans[li] = Some(OpenSpan {
                    line: out_lines.len(),
                    col_start: cur_col,
                    col_cur: cur_col,
                });
            }
        }

        // Handle hard breaks embedded in text.
        let mut text = run.text.as_str();
        while !text.is_empty() {
            // Hard break.
            if let Some(idx) = text.find('\n') {
                let head = &text[..idx];
                emit_segment(
                    head,
                    run,
                    &mut cur_spans,
                    &mut cur_col,
                    &mut cur_inner,
                    &mut at_line_start,
                    active_inner_width,
                    out_lines,
                    hanging,
                    cont_inner_width,
                    out_links,
                    open_spans,
                    &mut current_link,
                    links,
                    &mut active_inner_width,
                );
                // Force break.
                break_line(
                    out_lines,
                    &mut cur_spans,
                    &mut cur_col,
                    &mut cur_inner,
                    &mut active_inner_width,
                    &mut at_line_start,
                    out_links,
                    open_spans,
                    &mut current_link,
                    links,
                    hanging,
                    cont_inner_width,
                );
                text = &text[idx + 1..];
            } else {
                emit_segment(
                    text,
                    run,
                    &mut cur_spans,
                    &mut cur_col,
                    &mut cur_inner,
                    &mut at_line_start,
                    active_inner_width,
                    out_lines,
                    hanging,
                    cont_inner_width,
                    out_links,
                    open_spans,
                    &mut current_link,
                    links,
                    &mut active_inner_width,
                );
                text = "";
            }
        }
        let _ = push_run_chunk;
    }

    // Close any open link.
    if let Some(li) = current_link {
        if let Some(open) = open_spans[li].take() {
            let line = out_lines.len();
            if open.line == line && open.col_cur > open.col_start {
                out_links.push(LinkSpan {
                    line: open.line,
                    col_start: open.col_start,
                    col_end: open.col_cur,
                    target: links[li].target.clone(),
                });
            }
        }
    }

    out_lines.push(Line::from(cur_spans));
}

fn emit_segment(
    text: &str,
    run: &Run,
    cur_spans: &mut Vec<Span<'static>>,
    cur_col: &mut usize,
    cur_inner: &mut usize,
    at_line_start: &mut bool,
    _initial_inner_width: usize,
    out_lines: &mut Vec<Line<'static>>,
    hanging: &[Run],
    cont_inner_width: usize,
    out_links: &mut Vec<LinkSpan>,
    open_spans: &mut [Option<OpenSpan>],
    current_link: &mut Option<usize>,
    links: &[PendingLink],
    active_inner_width: &mut usize,
) {
    // Word-wrap the text on whitespace boundaries.
    let mut remaining = text;
    while !remaining.is_empty() {
        // Take a chunk: either whitespace run or non-whitespace word.
        let is_space = remaining.chars().next().unwrap().is_whitespace();
        let end = remaining
            .char_indices()
            .find(|(_, c)| c.is_whitespace() != is_space)
            .map(|(i, _)| i)
            .unwrap_or(remaining.len());
        let (word, rest) = remaining.split_at(end);
        let w = word.width();

        if is_space {
            if *at_line_start {
                // Skip leading whitespace on a wrapped line.
                remaining = rest;
                continue;
            }
            if *cur_inner + w > *active_inner_width {
                // Drop trailing whitespace and break.
                break_line_inline(
                    out_lines, cur_spans, cur_col, cur_inner, at_line_start,
                    active_inner_width, out_links, open_spans, current_link,
                    links, hanging, cont_inner_width,
                );
                remaining = rest;
                continue;
            }
            push_chunk(cur_spans, cur_col, cur_inner, run, word, current_link, open_spans);
            *at_line_start = false;
            remaining = rest;
            continue;
        }

        // Non-whitespace word.
        if w > *active_inner_width {
            // Word longer than line: hard-split by chars.
            let mut chars_left = word;
            while !chars_left.is_empty() {
                let avail = active_inner_width.saturating_sub(*cur_inner);
                if avail == 0 && !*at_line_start {
                    break_line_inline(
                        out_lines, cur_spans, cur_col, cur_inner, at_line_start,
                        active_inner_width, out_links, open_spans, current_link,
                        links, hanging, cont_inner_width,
                    );
                    continue;
                }
                let mut taken = 0usize;
                let mut taken_w = 0usize;
                for (i, c) in chars_left.char_indices() {
                    let cw = c.to_string().width();
                    if taken_w + cw > avail.max(1) { break; }
                    taken = i + c.len_utf8();
                    taken_w += cw;
                }
                if taken == 0 { taken = chars_left.chars().next().unwrap().len_utf8(); }
                let (head, tail) = chars_left.split_at(taken);
                push_chunk(cur_spans, cur_col, cur_inner, run, head, current_link, open_spans);
                *at_line_start = false;
                if !tail.is_empty() {
                    break_line_inline(
                        out_lines, cur_spans, cur_col, cur_inner, at_line_start,
                        active_inner_width, out_links, open_spans, current_link,
                        links, hanging, cont_inner_width,
                    );
                }
                chars_left = tail;
            }
            remaining = rest;
            continue;
        }

        if *cur_inner + w > *active_inner_width && !*at_line_start {
            break_line_inline(
                out_lines, cur_spans, cur_col, cur_inner, at_line_start,
                active_inner_width, out_links, open_spans, current_link,
                links, hanging, cont_inner_width,
            );
        }
        push_chunk(cur_spans, cur_col, cur_inner, run, word, current_link, open_spans);
        *at_line_start = false;
        remaining = rest;
    }
}

fn push_chunk(
    cur_spans: &mut Vec<Span<'static>>,
    cur_col: &mut usize,
    cur_inner: &mut usize,
    run: &Run,
    chunk: &str,
    current_link: &Option<usize>,
    open_spans: &mut [Option<OpenSpan>],
) {
    if chunk.is_empty() { return; }
    let w = chunk.width();
    cur_spans.push(Span::styled(chunk.to_string(), run.style));
    *cur_col += w;
    *cur_inner += w;
    if let Some(li) = current_link {
        if let Some(open) = &mut open_spans[*li] {
            open.col_cur = *cur_col;
        }
    }
}

fn break_line_inline(
    out_lines: &mut Vec<Line<'static>>,
    cur_spans: &mut Vec<Span<'static>>,
    cur_col: &mut usize,
    cur_inner: &mut usize,
    at_line_start: &mut bool,
    active_inner_width: &mut usize,
    out_links: &mut Vec<LinkSpan>,
    open_spans: &mut [Option<OpenSpan>],
    current_link: &mut Option<usize>,
    links: &[PendingLink],
    hanging: &[Run],
    cont_inner_width: usize,
) {
    if let Some(li) = *current_link {
        if let Some(open) = open_spans[li].take() {
            let line = out_lines.len();
            if open.line == line && open.col_cur > open.col_start {
                out_links.push(LinkSpan {
                    line: open.line,
                    col_start: open.col_start,
                    col_end: open.col_cur,
                    target: links[li].target.clone(),
                });
            }
        }
    }
    out_lines.push(Line::from(std::mem::take(cur_spans)));
    for r in hanging {
        cur_spans.push(Span::styled(r.text.clone(), r.style));
    }
    *cur_col = hanging.iter().map(|r| r.text.width()).sum();
    *cur_inner = 0;
    *active_inner_width = cont_inner_width;
    *at_line_start = true;
    if let Some(li) = *current_link {
        open_spans[li] = Some(OpenSpan {
            line: out_lines.len(),
            col_start: *cur_col,
            col_cur: *cur_col,
        });
    }
}

// ---------------------------------------------------------------------------
// Style helper: optional bg.
// ---------------------------------------------------------------------------

trait StyleExt {
    fn bg_opt(self, c: Option<Color>) -> Style;
}
impl StyleExt for Style {
    fn bg_opt(self, c: Option<Color>) -> Style {
        match c { Some(col) => self.bg(col), None => self }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::links::LinkTarget;
    use crate::theme::Theme;

    #[test]
    fn renders_paragraph_and_link() {
        let src = "hello [there](https://example.com) world";
        let r = render(src, None, 80, &Theme::dark());
        assert!(!r.lines.is_empty());
        assert_eq!(r.link_map.links.len(), 1);
        let link = &r.link_map.links[0];
        assert_eq!(link.target, LinkTarget::Url("https://example.com".to_string()));
        assert!(link.col_end > link.col_start);
    }

    #[test]
    fn records_heading_anchors() {
        let src = "# Hello World\n\nbody";
        let r = render(src, None, 80, &Theme::dark());
        assert_eq!(r.link_map.anchors.get("hello-world"), Some(&1));
    }

    #[test]
    fn wraps_long_paragraph() {
        let src = "one two three four five six seven eight nine ten";
        let r = render(src, None, 12, &Theme::dark());
        // "one two three" (13) needs wrapping in ≤12-wide column.
        assert!(r.lines.len() > 2);
    }

    #[test]
    fn link_span_split_across_wrapped_lines() {
        let src = "[long link text that spans multiple wrapped output lines](https://x)";
        let r = render(src, None, 16, &Theme::dark());
        assert!(r.link_map.links.len() >= 2);
        for l in &r.link_map.links {
            assert_eq!(l.target, LinkTarget::Url("https://x".to_string()));
        }
    }
}
