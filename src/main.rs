use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsStr;
use std::fs;
use std::io::{self, IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use crossterm::event::{self, Event as CEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::{execute, ExecutableCommand};
use notify::{Config, Event, RecommendedWatcher, RecursiveMode, Watcher};
use pulldown_cmark::{
    Alignment, CodeBlockKind, Event as MdEvent, HeadingLevel, Options, Parser as MdParser, Tag,
    TagEnd,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout};
use ratatui::prelude::{Color, Modifier, Rect, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::block::Padding;
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
use ratatui::Terminal;
use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

const NO_TOC_HEADINGS_STATUS: &str = "No headings in TOC";
const TIMELINE_DEFAULT_HEIGHT: u16 = 6;
const TIMELINE_MIN_HEIGHT: u16 = 3;
const BRIGHT_CHANGE_WINDOW: Duration = Duration::from_secs(2);
const DIM_CHANGE_WINDOW: Duration = Duration::from_secs(15);
const DIFF_MAX_CELLS: usize = 2_000_000;

fn system_open<S: AsRef<OsStr>>(arg: S) -> Result<()> {
    #[cfg(target_os = "macos")]
    let status = Command::new("open").arg(arg).status()?;

    #[cfg(all(unix, not(target_os = "macos")))]
    let status = Command::new("xdg-open").arg(arg).status()?;

    #[cfg(target_os = "windows")]
    let status = Command::new("cmd")
        .args(["/C", "start", ""])
        .arg(arg)
        .status()?;

    if !status.success() {
        return Err(anyhow!("system open command failed with status {status}"));
    }
    Ok(())
}

fn inset_rect(area: Rect, horizontal: u16, vertical: u16) -> Rect {
    let x = area.x.saturating_add(horizontal);
    let y = area.y.saturating_add(vertical);
    let width = area.width.saturating_sub(horizontal.saturating_mul(2));
    let height = area.height.saturating_sub(vertical.saturating_mul(2));
    Rect {
        x,
        y,
        width,
        height,
    }
}

fn usize_to_u16_saturating(value: usize) -> u16 {
    match u16::try_from(value) {
        Ok(v) => v,
        Err(_) => u16::MAX,
    }
}

fn parse_history(value: &str) -> std::result::Result<usize, String> {
    let parsed: usize = value
        .parse()
        .map_err(|_| "--history must be a positive integer".to_string())?;
    if parsed == 0 {
        return Err("--history must be at least 1".to_string());
    }
    Ok(parsed)
}

#[derive(Debug, Parser)]
#[command(
    name = "catmd",
    version,
    about = "Render markdown for terminal workflows"
)]
struct Cli {
    /// Markdown file path. Use '-' to read from stdin.
    input: Option<String>,

    /// Force interactive pager mode.
    #[arg(short, long)]
    interactive: bool,

    /// Force plain stdout rendering.
    #[arg(long)]
    plain: bool,

    /// Reload when the file changes (file input only).
    #[arg(long)]
    watch: bool,

    /// Number of in-memory snapshots to keep while watching.
    #[arg(long, default_value_t = 50, value_parser = parse_history)]
    history: usize,
}

#[derive(Clone, Debug)]
struct StyledSegment {
    text: String,
    style: Style,
}

#[derive(Clone, Debug, Default)]
struct RenderedLine {
    segments: Vec<StyledSegment>,
    plain: String,
}

#[derive(Clone, Debug)]
struct LinkRef {
    label: String,
    target: String,
    line: usize,
}

#[derive(Clone, Debug)]
struct TocEntry {
    level: u8,
    title: String,
    line: usize,
}

#[derive(Clone, Debug, Default)]
struct RenderedDocument {
    lines: Vec<RenderedLine>,
    toc: Vec<TocEntry>,
    links: Vec<LinkRef>,
}

#[derive(Clone, Debug)]
struct LoadedDocument {
    path: Option<PathBuf>,
    rendered: RenderedDocument,
}

#[derive(Debug)]
struct HistoryEntry {
    path: PathBuf,
    scroll: u16,
}

#[derive(Clone, Copy, Debug, Default)]
struct SectionDelta {
    added: usize,
    removed: usize,
}

#[derive(Clone, Debug, Default)]
struct DiffHunk {
    start_line: usize,
    end_line: usize,
    added: usize,
    removed: usize,
}

#[derive(Clone, Debug, Default)]
struct SnapshotDiff {
    added: usize,
    removed: usize,
    hunks: Vec<DiffHunk>,
    section_deltas: BTreeMap<usize, SectionDelta>,
    top_section: Option<String>,
    overflow: bool,
}

#[derive(Clone, Debug)]
struct WatchSnapshot {
    revision: u64,
    created_at: SystemTime,
    created_instant: Instant,
    rendered: RenderedDocument,
    diff: SnapshotDiff,
}

struct FileWatcher {
    _watcher: RecommendedWatcher,
    rx: Receiver<notify::Result<Event>>,
}

#[derive(Clone)]
struct ActiveLink {
    target: String,
    text: String,
}

#[derive(Clone)]
struct ActiveImage {
    target: String,
    alt: String,
}

#[derive(Default)]
struct TableState {
    in_head: bool,
    in_row: bool,
    in_cell: bool,
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
    current_row: Vec<String>,
    current_cell: String,
    alignments: Vec<Alignment>,
}

impl TableState {
    fn new(alignments: Vec<Alignment>) -> Self {
        Self {
            alignments,
            ..Self::default()
        }
    }
}

#[derive(Default)]
struct InlineState {
    emphasis: usize,
    strong: usize,
    strikethrough: usize,
    link_depth: usize,
}

impl InlineState {
    fn style(&self) -> Style {
        let mut style = Style::default();
        if self.emphasis > 0 {
            style = style.add_modifier(Modifier::ITALIC);
        }
        if self.strong > 0 {
            style = style.add_modifier(Modifier::BOLD);
        }
        if self.strikethrough > 0 {
            style = style.add_modifier(Modifier::CROSSED_OUT);
        }
        if self.link_depth > 0 {
            style = style.fg(Color::Cyan).add_modifier(Modifier::UNDERLINED);
        }
        style
    }
}

struct Renderer<'a> {
    syntax_set: &'a SyntaxSet,
    theme: &'a Theme,

    lines: Vec<RenderedLine>,
    toc: Vec<TocEntry>,
    links: Vec<LinkRef>,

    inline: InlineState,
    current_segments: Vec<StyledSegment>,
    current_plain: String,
    current_line_link_indices: Vec<usize>,

    active_link: Option<ActiveLink>,
    active_image: Option<ActiveImage>,

    heading_level: Option<u8>,
    blockquote_depth: usize,
    list_stack: Vec<ListState>,

    code_block_lang: Option<String>,
    code_block_buf: String,

    table: Option<TableState>,
}

#[derive(Clone, Debug)]
struct ListState {
    ordered: bool,
    next_index: u64,
}

impl<'a> Renderer<'a> {
    fn new(syntax_set: &'a SyntaxSet, theme: &'a Theme) -> Self {
        Self {
            syntax_set,
            theme,
            lines: Vec::new(),
            toc: Vec::new(),
            links: Vec::new(),
            inline: InlineState::default(),
            current_segments: Vec::new(),
            current_plain: String::new(),
            current_line_link_indices: Vec::new(),
            active_link: None,
            active_image: None,
            heading_level: None,
            blockquote_depth: 0,
            list_stack: Vec::new(),
            code_block_lang: None,
            code_block_buf: String::new(),
            table: None,
        }
    }

    fn finish(mut self) -> RenderedDocument {
        self.flush_line(false);
        if self.lines.is_empty() {
            self.lines.push(RenderedLine::default());
        }
        RenderedDocument {
            lines: self.lines,
            toc: self.toc,
            links: self.links,
        }
    }

    fn push_text(&mut self, text: &str, style: Style) {
        if text.is_empty() {
            return;
        }
        self.current_plain.push_str(text);
        self.current_segments.push(StyledSegment {
            text: text.to_string(),
            style,
        });
    }

    fn push_styled_plain_text(&mut self, text: &str) {
        let style = if let Some(level) = self.heading_level {
            match level {
                1 => Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
                2 => Style::default()
                    .fg(Color::LightMagenta)
                    .add_modifier(Modifier::BOLD),
                _ => Style::default()
                    .fg(Color::LightCyan)
                    .add_modifier(Modifier::BOLD),
            }
        } else {
            self.inline.style()
        };
        self.push_text(text, style);
    }

    fn push_prefix_if_needed(&mut self) {
        if !self.current_plain.is_empty() {
            return;
        }

        if self.blockquote_depth > 0 {
            let prefix = "> ".repeat(self.blockquote_depth);
            self.push_text(&prefix, Style::default().fg(Color::DarkGray));
        }
    }

    fn flush_line(&mut self, force_empty: bool) {
        if !force_empty && self.current_segments.is_empty() && self.current_plain.is_empty() {
            return;
        }

        let line_index = self.lines.len();
        for idx in &self.current_line_link_indices {
            if let Some(link) = self.links.get_mut(*idx) {
                link.line = line_index;
            }
        }

        let line = RenderedLine {
            segments: std::mem::take(&mut self.current_segments),
            plain: std::mem::take(&mut self.current_plain),
        };
        self.current_line_link_indices.clear();
        self.lines.push(line);
    }

    fn blank_line(&mut self) {
        if self.lines.last().is_some_and(|line| line.plain.is_empty()) {
            return;
        }
        self.flush_line(true);
    }

    fn heading_level_u8(level: HeadingLevel) -> u8 {
        match level {
            HeadingLevel::H1 => 1,
            HeadingLevel::H2 => 2,
            HeadingLevel::H3 => 3,
            HeadingLevel::H4 => 4,
            HeadingLevel::H5 => 5,
            HeadingLevel::H6 => 6,
        }
    }

    fn handle_start(&mut self, tag: Tag<'_>) {
        if let Some(table) = self.table.as_mut() {
            match tag {
                Tag::TableHead => {
                    table.in_head = true;
                    return;
                }
                Tag::TableRow => {
                    table.in_row = true;
                    table.current_row.clear();
                    return;
                }
                Tag::TableCell => {
                    table.in_cell = true;
                    table.current_cell.clear();
                    return;
                }
                _ => {}
            }
        }

        match tag {
            Tag::Heading { level, .. } => {
                self.flush_line(false);
                self.heading_level = Some(Self::heading_level_u8(level));
            }
            Tag::BlockQuote(_) => {
                self.flush_line(false);
                self.blockquote_depth = self.blockquote_depth.saturating_add(1);
            }
            Tag::CodeBlock(kind) => {
                self.flush_line(false);
                let lang = match kind {
                    CodeBlockKind::Fenced(name) => name.to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                self.code_block_lang = Some(lang);
                self.code_block_buf.clear();
            }
            Tag::List(start) => {
                let list = if let Some(index) = start {
                    ListState {
                        ordered: true,
                        next_index: index,
                    }
                } else {
                    ListState {
                        ordered: false,
                        next_index: 1,
                    }
                };
                self.list_stack.push(list);
            }
            Tag::Item => {
                self.flush_line(false);
                let depth = self.list_stack.len().saturating_sub(1);
                let indent = "  ".repeat(depth);

                let bullet = if let Some(last) = self.list_stack.last_mut() {
                    if last.ordered {
                        let bullet = format!("{}. ", last.next_index);
                        last.next_index = last.next_index.saturating_add(1);
                        bullet
                    } else {
                        "- ".to_string()
                    }
                } else {
                    "- ".to_string()
                };

                self.push_text(
                    &format!("{indent}{bullet}"),
                    Style::default().fg(Color::DarkGray),
                );
            }
            Tag::Emphasis => self.inline.emphasis = self.inline.emphasis.saturating_add(1),
            Tag::Strong => self.inline.strong = self.inline.strong.saturating_add(1),
            Tag::Strikethrough => {
                self.inline.strikethrough = self.inline.strikethrough.saturating_add(1);
            }
            Tag::Link { dest_url, .. } => {
                self.inline.link_depth = self.inline.link_depth.saturating_add(1);
                self.active_link = Some(ActiveLink {
                    target: dest_url.to_string(),
                    text: String::new(),
                });
            }
            Tag::Image { dest_url, .. } => {
                self.active_image = Some(ActiveImage {
                    target: dest_url.to_string(),
                    alt: String::new(),
                });
            }
            Tag::Table(alignments) => {
                self.flush_line(false);
                self.table = Some(TableState::new(alignments));
            }
            _ => {}
        }
    }

    fn handle_end(&mut self, tag: TagEnd) {
        if let Some(table) = self.table.as_mut() {
            match tag {
                TagEnd::TableCell => {
                    if table.in_cell {
                        table
                            .current_row
                            .push(table.current_cell.trim().to_string());
                        table.current_cell.clear();
                        table.in_cell = false;
                    }
                    return;
                }
                TagEnd::TableRow => {
                    if table.in_row {
                        if table.in_head {
                            table.headers = table.current_row.clone();
                        } else {
                            table.rows.push(table.current_row.clone());
                        }
                        table.current_row.clear();
                        table.in_row = false;
                    }
                    return;
                }
                TagEnd::TableHead => {
                    table.in_head = false;
                    return;
                }
                TagEnd::Table => {
                    let table_state = self.table.take().unwrap_or_default();
                    self.render_table(&table_state);
                    self.blank_line();
                    return;
                }
                _ => {}
            }
        }

        match tag {
            TagEnd::Paragraph => {
                self.flush_line(false);
                self.blank_line();
            }
            TagEnd::Heading(level) => {
                self.flush_line(false);
                let line_idx = self.lines.len().saturating_sub(1);
                let title = self
                    .lines
                    .get(line_idx)
                    .map(|line| line.plain.trim().to_string())
                    .unwrap_or_default();

                let level_u8 = Self::heading_level_u8(level);
                if level_u8 <= 3 && !title.is_empty() {
                    self.toc.push(TocEntry {
                        level: level_u8,
                        title,
                        line: line_idx,
                    });
                }
                self.heading_level = None;
                self.blank_line();
            }
            TagEnd::BlockQuote => {
                self.flush_line(false);
                self.blockquote_depth = self.blockquote_depth.saturating_sub(1);
                self.blank_line();
            }
            TagEnd::CodeBlock => {
                let lang = self.code_block_lang.take().unwrap_or_default();
                let code = std::mem::take(&mut self.code_block_buf);
                self.render_code_block(&lang, &code);
                self.blank_line();
            }
            TagEnd::List(_) => {
                self.flush_line(false);
                self.list_stack.pop();
                self.blank_line();
            }
            TagEnd::Item => {
                self.flush_line(false);
            }
            TagEnd::Emphasis => self.inline.emphasis = self.inline.emphasis.saturating_sub(1),
            TagEnd::Strong => self.inline.strong = self.inline.strong.saturating_sub(1),
            TagEnd::Strikethrough => {
                self.inline.strikethrough = self.inline.strikethrough.saturating_sub(1);
            }
            TagEnd::Link => {
                self.inline.link_depth = self.inline.link_depth.saturating_sub(1);
                if let Some(link) = self.active_link.take() {
                    let link_ref = LinkRef {
                        label: if link.text.trim().is_empty() {
                            link.target.clone()
                        } else {
                            link.text.trim().to_string()
                        },
                        target: link.target,
                        line: usize::MAX,
                    };
                    let index = self.links.len();
                    self.links.push(link_ref);
                    self.current_line_link_indices.push(index);
                }
            }
            TagEnd::Image => {
                if let Some(image) = self.active_image.take() {
                    let alt = if image.alt.trim().is_empty() {
                        "image".to_string()
                    } else {
                        image.alt.trim().to_string()
                    };
                    let placeholder = format!("[image: {alt}] ({})", image.target);
                    self.push_text(&placeholder, Style::default().fg(Color::LightBlue));
                }
            }
            _ => {}
        }
    }

    fn add_text(&mut self, text: &str) {
        if self.code_block_lang.is_some() {
            self.code_block_buf.push_str(text);
            return;
        }

        if let Some(table) = self.table.as_mut() {
            if table.in_cell {
                table.current_cell.push_str(text);
                return;
            }
        }

        if let Some(image) = self.active_image.as_mut() {
            image.alt.push_str(text);
            return;
        }

        self.push_prefix_if_needed();
        self.push_styled_plain_text(text);
        if let Some(link) = self.active_link.as_mut() {
            link.text.push_str(text);
        }
    }

    fn soft_break(&mut self) {
        if self.code_block_lang.is_some() {
            self.code_block_buf.push('\n');
            return;
        }
        if let Some(table) = self.table.as_mut() {
            if table.in_cell {
                table.current_cell.push(' ');
                return;
            }
        }

        self.push_text(" ", self.inline.style());
        if let Some(link) = self.active_link.as_mut() {
            link.text.push(' ');
        }
    }

    fn hard_break(&mut self) {
        if self.code_block_lang.is_some() {
            self.code_block_buf.push('\n');
            return;
        }
        self.flush_line(false);
    }

    fn add_inline_code(&mut self, code: &str) {
        if self.code_block_lang.is_some() {
            self.code_block_buf.push_str(code);
            return;
        }
        if let Some(table) = self.table.as_mut() {
            if table.in_cell {
                table.current_cell.push_str(code);
                return;
            }
        }
        self.push_prefix_if_needed();
        let style = Style::default()
            .fg(Color::LightYellow)
            .add_modifier(Modifier::BOLD);
        self.push_text(code, style);
        if let Some(link) = self.active_link.as_mut() {
            link.text.push_str(code);
        }
    }

    fn add_rule(&mut self) {
        self.flush_line(false);
        self.push_text(
            "────────────────────────────────────────────────────────────────",
            Style::default().fg(Color::DarkGray),
        );
        self.flush_line(false);
        self.blank_line();
    }

    fn add_task_marker(&mut self, done: bool) {
        self.push_prefix_if_needed();
        let marker = if done { "[x] " } else { "[ ] " };
        self.push_text(marker, Style::default().fg(Color::DarkGray));
    }

    fn render_code_block(&mut self, lang: &str, code: &str) {
        let syntax = if lang.trim().is_empty() {
            self.syntax_set.find_syntax_plain_text()
        } else {
            self.syntax_set
                .find_syntax_by_token(lang)
                .unwrap_or_else(|| self.syntax_set.find_syntax_plain_text())
        };

        let mut highlighter = HighlightLines::new(syntax, self.theme);

        for line in LinesWithEndings::from(code) {
            let mut clean = line;
            if let Some(trimmed) = clean.strip_suffix('\n') {
                clean = trimmed;
            }
            if let Some(trimmed) = clean.strip_suffix('\r') {
                clean = trimmed;
            }

            self.push_text("  ", Style::default().fg(Color::DarkGray));

            let highlighted_tokens = highlighter
                .highlight_line(line, self.syntax_set)
                .unwrap_or_default();

            if highlighted_tokens.is_empty() {
                self.push_text(clean, Style::default().fg(Color::LightGreen));
            } else {
                for (syn_style, token) in highlighted_tokens {
                    let style = Style::default()
                        .fg(Color::Rgb(
                            syn_style.foreground.r,
                            syn_style.foreground.g,
                            syn_style.foreground.b,
                        ))
                        .bg(Color::Rgb(
                            syn_style.background.r,
                            syn_style.background.g,
                            syn_style.background.b,
                        ));
                    let token = token.replace('\n', "");
                    self.push_text(&token, style);
                }
            }

            self.flush_line(false);
        }
    }

    fn render_table(&mut self, table: &TableState) {
        let mut rows: Vec<Vec<String>> = Vec::new();
        if !table.headers.is_empty() {
            rows.push(table.headers.clone());
        }
        rows.extend(table.rows.clone());

        if rows.is_empty() {
            return;
        }

        let col_count = rows.iter().map(Vec::len).max().unwrap_or(0);
        if col_count == 0 {
            return;
        }

        for row in &mut rows {
            while row.len() < col_count {
                row.push(String::new());
            }
        }

        let mut widths = vec![3usize; col_count];
        for row in &rows {
            for (idx, cell) in row.iter().enumerate() {
                widths[idx] = widths[idx].max(cell.chars().count());
            }
        }

        if let Some(header) = rows.first() {
            let line = Self::format_table_row(header, &widths);
            self.push_text(&line, Style::default().fg(Color::Yellow));
            self.flush_line(false);

            let mut sep_cells = Vec::with_capacity(col_count);
            for (idx, width) in widths.iter().enumerate() {
                let align = table
                    .alignments
                    .get(idx)
                    .copied()
                    .unwrap_or(Alignment::None);
                let sep = match align {
                    Alignment::Left => format!(":{}", "-".repeat(width.saturating_sub(1))),
                    Alignment::Center => {
                        if *width <= 1 {
                            ":".to_string()
                        } else {
                            format!(":{}:", "-".repeat(width.saturating_sub(2)))
                        }
                    }
                    Alignment::Right => format!("{}:", "-".repeat(width.saturating_sub(1))),
                    Alignment::None => "-".repeat(*width),
                };
                sep_cells.push(sep);
            }
            let sep_line = Self::format_table_row(&sep_cells, &widths);
            self.push_text(&sep_line, Style::default().fg(Color::DarkGray));
            self.flush_line(false);

            for row in rows.iter().skip(1) {
                let row_line = Self::format_table_row(row, &widths);
                self.push_text(&row_line, Style::default());
                self.flush_line(false);
            }
        }
    }

    fn format_table_row(row: &[String], widths: &[usize]) -> String {
        let mut output = String::from("| ");
        for (idx, cell) in row.iter().enumerate() {
            let width = widths[idx];
            let padded = format!("{cell:<width$}");
            output.push_str(&padded);
            output.push_str(" | ");
        }
        output
    }
}

fn render_markdown(source: &str, syntax_set: &SyntaxSet, theme: &Theme) -> RenderedDocument {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_FOOTNOTES);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_SMART_PUNCTUATION);

    let parser = MdParser::new_ext(source, options);
    let mut renderer = Renderer::new(syntax_set, theme);

    for event in parser {
        match event {
            MdEvent::Start(tag) => renderer.handle_start(tag),
            MdEvent::End(tag) => renderer.handle_end(tag),
            MdEvent::Text(text) => renderer.add_text(&text),
            MdEvent::Code(code) => renderer.add_inline_code(&code),
            MdEvent::Html(html) | MdEvent::InlineHtml(html) => renderer.add_text(&html),
            MdEvent::FootnoteReference(name) => renderer.add_text(&format!("[^{name}]")),
            MdEvent::SoftBreak => renderer.soft_break(),
            MdEvent::HardBreak => renderer.hard_break(),
            MdEvent::Rule => renderer.add_rule(),
            MdEvent::TaskListMarker(done) => renderer.add_task_marker(done),
            _ => {}
        }
    }

    renderer.finish()
}

#[derive(Clone)]
struct LoadResult {
    path: Option<PathBuf>,
    source: String,
}

enum InputSource {
    File(PathBuf),
    Stdin,
}

fn detect_input(cli: &Cli) -> Result<InputSource> {
    match cli.input.as_deref() {
        Some("-") => Ok(InputSource::Stdin),
        Some(path) => Ok(InputSource::File(PathBuf::from(path))),
        None => {
            if io::stdin().is_terminal() {
                Err(anyhow!(
                    "No input provided. Pass a markdown file or pipe markdown into stdin."
                ))
            } else {
                Ok(InputSource::Stdin)
            }
        }
    }
}

fn read_input(source: &InputSource) -> Result<LoadResult> {
    match source {
        InputSource::File(path) => {
            let source = fs::read_to_string(path)
                .with_context(|| format!("Failed to read {}", path.display()))?;
            Ok(LoadResult {
                path: Some(path.clone()),
                source,
            })
        }
        InputSource::Stdin => {
            let mut buf = String::new();
            io::stdin()
                .read_to_string(&mut buf)
                .context("Failed to read markdown from stdin")?;
            Ok(LoadResult {
                path: None,
                source: buf,
            })
        }
    }
}

fn is_tty_stdout() -> bool {
    io::stdout().is_terminal()
}

fn default_interactive(input: &InputSource) -> bool {
    matches!(input, InputSource::File(_)) && is_tty_stdout()
}

fn resolve_theme(theme_set: &ThemeSet) -> Theme {
    if let Some(theme) = theme_set.themes.get("base16-ocean.dark") {
        return theme.clone();
    }
    theme_set
        .themes
        .values()
        .next()
        .cloned()
        .unwrap_or_default()
}

fn plain_render(doc: &RenderedDocument) -> String {
    let mut out = String::new();
    for (idx, line) in doc.lines.iter().enumerate() {
        out.push_str(&line.plain);
        if idx + 1 < doc.lines.len() {
            out.push('\n');
        }
    }
    out
}

#[derive(Clone)]
enum LinkAction {
    InternalMarkdown(PathBuf),
    ExternalUrl(String),
    ExternalPath(PathBuf),
    Anchor(String),
    Unknown(String),
}

fn classify_link(target: &str, current_doc: Option<&Path>) -> LinkAction {
    if target.starts_with("http://") || target.starts_with("https://") {
        return LinkAction::ExternalUrl(target.to_string());
    }

    if target.starts_with('#') {
        return LinkAction::Anchor(target.to_string());
    }

    let (path_part, _fragment) = if let Some((path, frag)) = target.split_once('#') {
        (path, Some(frag))
    } else {
        (target, None)
    };

    if path_part.is_empty() {
        return LinkAction::Anchor(target.to_string());
    }

    let path = PathBuf::from(path_part);
    let resolved = if path.is_absolute() {
        path
    } else if let Some(doc) = current_doc {
        if let Some(parent) = doc.parent() {
            parent.join(path)
        } else {
            path
        }
    } else {
        PathBuf::from(path_part)
    };

    let ext = resolved
        .extension()
        .and_then(|s| s.to_str())
        .map(str::to_ascii_lowercase);

    if matches!(ext.as_deref(), Some("md" | "markdown" | "mdx")) {
        return LinkAction::InternalMarkdown(resolved);
    }

    if resolved.exists() {
        return LinkAction::ExternalPath(resolved);
    }

    LinkAction::Unknown(target.to_string())
}

#[derive(Debug, Default)]
struct LineDiffResult {
    added: usize,
    removed: usize,
    hunks: Vec<DiffHunk>,
    overflow: bool,
}

#[derive(Clone, Copy)]
enum DiffOp {
    Equal,
    Add,
    Remove,
}

#[derive(Clone, Copy)]
enum ChangeFreshness {
    Bright,
    Dim,
}

fn change_freshness(created_instant: Instant) -> Option<ChangeFreshness> {
    let age = created_instant.elapsed();
    if age <= BRIGHT_CHANGE_WINDOW {
        Some(ChangeFreshness::Bright)
    } else if age <= DIM_CHANGE_WINDOW {
        Some(ChangeFreshness::Dim)
    } else {
        None
    }
}

fn format_clock_hms(time: SystemTime) -> String {
    let seconds = time
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        % 86_400;
    let hour = seconds / 3_600;
    let minute = (seconds % 3_600) / 60;
    let second = seconds % 60;
    format!("{hour:02}:{minute:02}:{second:02}")
}

fn truncate_label(text: &str, max_chars: usize) -> String {
    let char_count = text.chars().count();
    if char_count <= max_chars {
        return text.to_string();
    }
    if max_chars <= 1 {
        return "…".to_string();
    }
    let mut out = String::new();
    for ch in text.chars().take(max_chars.saturating_sub(1)) {
        out.push(ch);
    }
    out.push('…');
    out
}

fn heading_index_for_line(toc: &[TocEntry], line: usize) -> Option<usize> {
    if toc.is_empty() {
        return None;
    }

    toc.iter()
        .enumerate()
        .rfind(|(_, entry)| entry.line <= line)
        .map(|(idx, _)| idx)
        .or(Some(0))
}

fn hunk_anchor_line(hunk: &DiffHunk, total_lines: usize) -> usize {
    if total_lines == 0 {
        return 0;
    }

    if hunk.end_line > hunk.start_line {
        hunk.start_line.min(total_lines.saturating_sub(1))
    } else {
        hunk.start_line
            .saturating_sub(1)
            .min(total_lines.saturating_sub(1))
    }
}

fn touched_toc_indices_for_hunk(
    hunk: &DiffHunk,
    toc: &[TocEntry],
    total_lines: usize,
) -> Vec<usize> {
    if toc.is_empty() {
        return Vec::new();
    }

    let start_line = if total_lines == 0 {
        0
    } else {
        hunk.start_line.min(total_lines.saturating_sub(1))
    };
    let end_line = if total_lines == 0 {
        0
    } else if hunk.end_line > hunk.start_line {
        hunk.end_line
            .saturating_sub(1)
            .min(total_lines.saturating_sub(1))
    } else {
        hunk.start_line
            .saturating_sub(1)
            .min(total_lines.saturating_sub(1))
    };

    let from = start_line.min(end_line);
    let to = start_line.max(end_line);
    let Some(start_idx) = heading_index_for_line(toc, from) else {
        return Vec::new();
    };
    let end_idx = heading_index_for_line(toc, to).unwrap_or(start_idx);
    (start_idx..=end_idx).collect()
}

fn build_snapshot_diff(previous: &RenderedDocument, next: &RenderedDocument) -> SnapshotDiff {
    let old_lines: Vec<&str> = previous
        .lines
        .iter()
        .map(|line| line.plain.as_str())
        .collect();
    let new_lines: Vec<&str> = next.lines.iter().map(|line| line.plain.as_str()).collect();
    let line_diff = compute_line_diff(&old_lines, &new_lines, DIFF_MAX_CELLS);

    let mut section_deltas: BTreeMap<usize, SectionDelta> = BTreeMap::new();
    for hunk in &line_diff.hunks {
        let touched = touched_toc_indices_for_hunk(hunk, &next.toc, next.lines.len());
        for idx in &touched {
            section_deltas.entry(*idx).or_default();
        }
        if let Some(primary) = touched.first() {
            let entry = section_deltas.entry(*primary).or_default();
            entry.added = entry.added.saturating_add(hunk.added);
            entry.removed = entry.removed.saturating_add(hunk.removed);
        }
    }

    let top_section = section_deltas
        .first_key_value()
        .and_then(|(idx, _)| next.toc.get(*idx).map(|entry| entry.title.clone()));

    SnapshotDiff {
        added: line_diff.added,
        removed: line_diff.removed,
        hunks: line_diff.hunks,
        section_deltas,
        top_section,
        overflow: line_diff.overflow,
    }
}

fn compute_line_diff(old_lines: &[&str], new_lines: &[&str], max_cells: usize) -> LineDiffResult {
    let mut prefix = 0usize;
    while prefix < old_lines.len()
        && prefix < new_lines.len()
        && old_lines[prefix] == new_lines[prefix]
    {
        prefix += 1;
    }

    let mut old_end = old_lines.len();
    let mut new_end = new_lines.len();
    while old_end > prefix
        && new_end > prefix
        && old_lines[old_end.saturating_sub(1)] == new_lines[new_end.saturating_sub(1)]
    {
        old_end = old_end.saturating_sub(1);
        new_end = new_end.saturating_sub(1);
    }

    let old_mid = &old_lines[prefix..old_end];
    let new_mid = &new_lines[prefix..new_end];

    if old_mid.is_empty() && new_mid.is_empty() {
        return LineDiffResult::default();
    }

    if old_mid.is_empty() {
        return LineDiffResult {
            added: new_mid.len(),
            removed: 0,
            hunks: vec![DiffHunk {
                start_line: prefix,
                end_line: prefix.saturating_add(new_mid.len()),
                added: new_mid.len(),
                removed: 0,
            }],
            overflow: false,
        };
    }

    if new_mid.is_empty() {
        return LineDiffResult {
            added: 0,
            removed: old_mid.len(),
            hunks: vec![DiffHunk {
                start_line: prefix,
                end_line: prefix,
                added: 0,
                removed: old_mid.len(),
            }],
            overflow: false,
        };
    }

    let rows = old_mid.len().saturating_add(1);
    let cols = new_mid.len().saturating_add(1);
    if rows.saturating_mul(cols) > max_cells {
        return LineDiffResult {
            added: new_mid.len(),
            removed: old_mid.len(),
            hunks: vec![DiffHunk {
                start_line: prefix,
                end_line: prefix.saturating_add(new_mid.len()),
                added: new_mid.len(),
                removed: old_mid.len(),
            }],
            overflow: true,
        };
    }

    let mut table = vec![0u32; rows.saturating_mul(cols)];
    for i in 1..rows {
        for j in 1..cols {
            let idx = i.saturating_mul(cols).saturating_add(j);
            table[idx] = if old_mid[i.saturating_sub(1)] == new_mid[j.saturating_sub(1)] {
                table[(i.saturating_sub(1))
                    .saturating_mul(cols)
                    .saturating_add(j.saturating_sub(1))]
                .saturating_add(1)
            } else {
                table[(i.saturating_sub(1)).saturating_mul(cols).saturating_add(j)]
                    .max(table[i.saturating_mul(cols).saturating_add(j.saturating_sub(1))])
            };
        }
    }

    let mut ops_reversed = Vec::with_capacity(old_mid.len().saturating_add(new_mid.len()));
    let mut i = old_mid.len();
    let mut j = new_mid.len();

    while i > 0 && j > 0 {
        if old_mid[i.saturating_sub(1)] == new_mid[j.saturating_sub(1)] {
            ops_reversed.push(DiffOp::Equal);
            i = i.saturating_sub(1);
            j = j.saturating_sub(1);
            continue;
        }

        let up = table[(i.saturating_sub(1)).saturating_mul(cols).saturating_add(j)];
        let left = table[i.saturating_mul(cols).saturating_add(j.saturating_sub(1))];
        if up >= left {
            ops_reversed.push(DiffOp::Remove);
            i = i.saturating_sub(1);
        } else {
            ops_reversed.push(DiffOp::Add);
            j = j.saturating_sub(1);
        }
    }

    while i > 0 {
        ops_reversed.push(DiffOp::Remove);
        i = i.saturating_sub(1);
    }
    while j > 0 {
        ops_reversed.push(DiffOp::Add);
        j = j.saturating_sub(1);
    }

    ops_reversed.reverse();

    let mut hunks = Vec::new();
    let mut current: Option<DiffHunk> = None;
    let mut new_index = prefix;
    let mut added = 0usize;
    let mut removed = 0usize;

    for op in ops_reversed {
        match op {
            DiffOp::Equal => {
                new_index = new_index.saturating_add(1);
                if let Some(hunk) = current.take() {
                    hunks.push(hunk);
                }
            }
            DiffOp::Add => {
                added = added.saturating_add(1);
                let hunk = current.get_or_insert(DiffHunk {
                    start_line: new_index,
                    end_line: new_index,
                    added: 0,
                    removed: 0,
                });
                hunk.added = hunk.added.saturating_add(1);
                new_index = new_index.saturating_add(1);
                hunk.end_line = new_index;
            }
            DiffOp::Remove => {
                removed = removed.saturating_add(1);
                let hunk = current.get_or_insert(DiffHunk {
                    start_line: new_index,
                    end_line: new_index,
                    added: 0,
                    removed: 0,
                });
                hunk.removed = hunk.removed.saturating_add(1);
            }
        }
    }

    if let Some(hunk) = current.take() {
        hunks.push(hunk);
    }

    LineDiffResult {
        added,
        removed,
        hunks,
        overflow: false,
    }
}

struct App {
    cli: Cli,
    syntax_set: SyntaxSet,
    theme: Theme,
    doc: LoadedDocument,
    snapshots: VecDeque<WatchSnapshot>,
    active_snapshot: usize,
    next_revision: u64,
    history_capacity: usize,

    scroll: u16,
    viewport_height: u16,
    toc_open: bool,
    toc_selected: usize,
    timeline_open: bool,
    timeline_height: u16,

    selected_link: Option<usize>,
    backstack: Vec<HistoryEntry>,

    search_mode: bool,
    search_query: String,
    search_matches: Vec<usize>,
    current_match: usize,

    status: String,

    watcher: Option<FileWatcher>,
    watch_requested: bool,
}

impl App {
    fn new(
        cli: Cli,
        load: LoadResult,
        rendered: RenderedDocument,
        syntax_set: SyntaxSet,
        theme: Theme,
    ) -> Self {
        let selected_link = if rendered.links.is_empty() {
            None
        } else {
            Some(0)
        };
        let mut snapshots = VecDeque::new();
        snapshots.push_back(WatchSnapshot {
            revision: 1,
            created_at: SystemTime::now(),
            created_instant: Instant::now(),
            rendered: rendered.clone(),
            diff: SnapshotDiff::default(),
        });

        let history_capacity = cli.history.max(1);

        Self {
            cli,
            syntax_set,
            theme,
            doc: LoadedDocument {
                path: load.path,
                rendered,
            },
            snapshots,
            active_snapshot: 0,
            next_revision: 2,
            history_capacity,
            scroll: 0,
            viewport_height: 1,
            toc_open: false,
            toc_selected: 0,
            timeline_open: false,
            timeline_height: TIMELINE_DEFAULT_HEIGHT,
            selected_link,
            backstack: Vec::new(),
            search_mode: false,
            search_query: String::new(),
            search_matches: Vec::new(),
            current_match: 0,
            status: String::new(),
            watcher: None,
            watch_requested: false,
        }
    }

    fn latest_snapshot_index(&self) -> usize {
        self.snapshots.len().saturating_sub(1)
    }

    fn current_snapshot(&self) -> Option<&WatchSnapshot> {
        self.snapshots.get(self.active_snapshot)
    }

    fn is_live_mode(&self) -> bool {
        self.active_snapshot == self.latest_snapshot_index()
    }

    fn reset_snapshots_from_current_doc(&mut self) {
        let revision = self.next_revision;
        self.next_revision = self.next_revision.saturating_add(1);
        self.snapshots.clear();
        self.snapshots.push_back(WatchSnapshot {
            revision,
            created_at: SystemTime::now(),
            created_instant: Instant::now(),
            rendered: self.doc.rendered.clone(),
            diff: SnapshotDiff::default(),
        });
        self.active_snapshot = 0;
    }

    fn sync_doc_with_active_snapshot(&mut self, old_scroll: u16, fallback_to_first_hunk: bool) {
        let Some(snapshot) = self.current_snapshot().cloned() else {
            return;
        };

        self.doc.rendered = snapshot.rendered;
        self.selected_link = if self.doc.rendered.links.is_empty() {
            None
        } else {
            Some(0)
        };

        self.update_search_matches();
        if self.search_query.is_empty() || self.search_matches.is_empty() {
            if old_scroll <= self.max_scroll() {
                self.scroll = old_scroll;
            } else if fallback_to_first_hunk {
                if let Some(hunk) = snapshot.diff.hunks.first() {
                    self.set_scroll_to_line(hunk_anchor_line(hunk, self.doc.rendered.lines.len()));
                } else {
                    self.scroll = self.max_scroll();
                }
            } else {
                self.scroll = self.max_scroll();
            }
        }

        self.clamp_scroll();
        self.sync_toc_selected_with_scroll();
    }

    fn push_watch_snapshot(&mut self, rendered: RenderedDocument) -> bool {
        let diff = self
            .snapshots
            .back()
            .map(|previous| build_snapshot_diff(&previous.rendered, &rendered))
            .unwrap_or_default();

        if diff.hunks.is_empty() && diff.added == 0 && diff.removed == 0 {
            return false;
        }

        let was_live = self.is_live_mode();
        let old_scroll = self.scroll;
        let revision = self.next_revision;
        self.next_revision = self.next_revision.saturating_add(1);

        self.snapshots.push_back(WatchSnapshot {
            revision,
            created_at: SystemTime::now(),
            created_instant: Instant::now(),
            rendered,
            diff,
        });

        let mut selected_evicted = false;
        while self.snapshots.len() > self.history_capacity {
            self.snapshots.pop_front();
            if self.active_snapshot > 0 {
                self.active_snapshot = self.active_snapshot.saturating_sub(1);
            } else {
                selected_evicted = true;
            }
        }

        if was_live {
            self.active_snapshot = self.latest_snapshot_index();
            self.sync_doc_with_active_snapshot(old_scroll, true);
        } else if selected_evicted {
            self.sync_doc_with_active_snapshot(old_scroll, true);
        }

        true
    }

    fn toggle_timeline(&mut self) {
        if !self.cli.watch {
            self.status = "Timeline is available only in --watch mode".to_string();
            return;
        }
        self.timeline_open = !self.timeline_open;
    }

    fn move_revision_relative(&mut self, older: bool) {
        if !self.cli.watch {
            self.status = "Revision navigation is available only in --watch mode".to_string();
            return;
        }
        if self.snapshots.len() <= 1 {
            self.status = "No prior revisions yet".to_string();
            return;
        }

        let next_index = if older {
            self.active_snapshot.saturating_sub(1)
        } else {
            self.active_snapshot
                .saturating_add(1)
                .min(self.latest_snapshot_index())
        };

        if next_index == self.active_snapshot {
            self.status = if older {
                "Already at oldest revision".to_string()
            } else {
                "Already at latest revision".to_string()
            };
            return;
        }

        let old_scroll = self.scroll;
        self.active_snapshot = next_index;
        self.sync_doc_with_active_snapshot(old_scroll, true);

        if let Some(snapshot) = self.current_snapshot() {
            let behind = self
                .latest_snapshot_index()
                .saturating_sub(self.active_snapshot);
            if behind == 0 {
                self.status = format!("LIVE r{:03}", snapshot.revision);
            } else {
                self.status = format!("HISTORY r{:03} ({behind} behind LIVE)", snapshot.revision);
            }
        }
    }

    fn jump_to_live_revision(&mut self) {
        if !self.cli.watch {
            self.status = "Jump-to-live is available only in --watch mode".to_string();
            return;
        }
        if self.snapshots.is_empty() {
            return;
        }
        if self.is_live_mode() {
            self.status = "Already on LIVE revision".to_string();
            return;
        }

        let old_scroll = self.scroll;
        self.active_snapshot = self.latest_snapshot_index();
        self.sync_doc_with_active_snapshot(old_scroll, true);
        if let Some(snapshot) = self.current_snapshot() {
            self.status = format!("Returned to LIVE r{:03}", snapshot.revision);
        }
    }

    fn jump_hunk_relative(&mut self, reverse: bool) {
        let Some(snapshot) = self.current_snapshot().cloned() else {
            self.status = "No active revision".to_string();
            return;
        };
        if snapshot.diff.hunks.is_empty() {
            self.status = "No changed hunks in selected revision".to_string();
            return;
        }

        let total_lines = self.doc.rendered.lines.len();
        let anchors: Vec<usize> = snapshot
            .diff
            .hunks
            .iter()
            .map(|hunk| hunk_anchor_line(hunk, total_lines))
            .collect();

        let cursor = usize::from(self.scroll);
        let target = if reverse {
            anchors
                .iter()
                .rfind(|line| **line < cursor)
                .copied()
                .unwrap_or_else(|| *anchors.last().unwrap_or(&0))
        } else {
            anchors
                .iter()
                .find(|line| **line > cursor)
                .copied()
                .unwrap_or_else(|| anchors.first().copied().unwrap_or(0))
        };

        self.set_scroll_to_line(target);
        let hunk_number = anchors
            .iter()
            .position(|line| *line == target)
            .map(|idx| idx.saturating_add(1))
            .unwrap_or(1);
        self.status = format!("Hunk {hunk_number}/{}", anchors.len());
    }

    fn max_scroll(&self) -> u16 {
        let total = self.doc.rendered.lines.len();
        let visible = self.viewport_height.max(1) as usize;
        usize_to_u16_saturating(total.saturating_sub(visible))
    }

    fn set_scroll_and_sync(&mut self, scroll: u16) {
        self.scroll = scroll.min(self.max_scroll());
        self.sync_toc_selected_with_scroll();
    }

    fn set_scroll_to_line(&mut self, line: usize) {
        self.set_scroll_and_sync(usize_to_u16_saturating(line));
    }

    fn clamp_scroll(&mut self) {
        self.scroll = self.scroll.min(self.max_scroll());
    }

    fn selected_link_line(&self) -> Option<usize> {
        self.selected_link
            .and_then(|idx| self.doc.rendered.links.get(idx))
            .map(|link| link.line)
    }

    fn sync_toc_selected_with_scroll(&mut self) {
        self.toc_selected = self
            .doc
            .rendered
            .toc
            .iter()
            .rposition(|entry| entry.line <= usize::from(self.scroll))
            .unwrap_or(0);
    }

    fn move_toc_selection(&mut self, reverse: bool) {
        let len = self.doc.rendered.toc.len();
        if len == 0 {
            self.toc_selected = 0;
            self.status = NO_TOC_HEADINGS_STATUS.to_string();
            return;
        }
        if reverse {
            self.toc_selected = self.toc_selected.saturating_sub(1);
        } else {
            self.toc_selected = (self.toc_selected + 1).min(len.saturating_sub(1));
        }
    }

    fn jump_to_toc_index(&mut self, index: usize) {
        if let Some((line, title)) = self
            .doc
            .rendered
            .toc
            .get(index)
            .map(|entry| (entry.line, entry.title.clone()))
        {
            self.toc_selected = index;
            self.set_scroll_to_line(line);
            self.status = format!("Jumped to {title}");
        } else {
            self.status = NO_TOC_HEADINGS_STATUS.to_string();
        }
    }

    fn jump_to_toc_selected(&mut self) {
        self.jump_to_toc_index(self.toc_selected);
    }

    fn jump_heading_relative(&mut self, reverse: bool) {
        let toc = &self.doc.rendered.toc;
        if toc.is_empty() {
            self.status = NO_TOC_HEADINGS_STATUS.to_string();
            return;
        }

        let line = usize::from(self.scroll);
        let target_index = if reverse {
            toc.iter()
                .enumerate()
                .rev()
                .find(|(_, entry)| entry.line < line)
                .map_or(0, |(idx, _)| idx)
        } else {
            toc.iter()
                .enumerate()
                .find(|(_, entry)| entry.line > line)
                .map_or_else(|| toc.len().saturating_sub(1), |(idx, _)| idx)
        };

        self.jump_to_toc_index(target_index);
    }

    fn update_search_matches(&mut self) {
        if self.search_query.is_empty() {
            self.search_matches.clear();
            self.current_match = 0;
            return;
        }

        let needle = self.search_query.to_ascii_lowercase();
        self.search_matches = self
            .doc
            .rendered
            .lines
            .iter()
            .enumerate()
            .filter_map(|(idx, line)| {
                if line.plain.to_ascii_lowercase().contains(&needle) {
                    Some(idx)
                } else {
                    None
                }
            })
            .collect();

        if self.search_matches.is_empty() {
            self.current_match = 0;
            return;
        }

        self.current_match = self
            .current_match
            .min(self.search_matches.len().saturating_sub(1));
        self.set_scroll_to_line(self.search_matches[self.current_match]);
    }

    fn jump_to_next_match(&mut self, reverse: bool) {
        if self.search_matches.is_empty() {
            return;
        }

        if reverse {
            if self.current_match == 0 {
                self.current_match = self.search_matches.len().saturating_sub(1);
            } else {
                self.current_match -= 1;
            }
        } else {
            self.current_match = (self.current_match + 1) % self.search_matches.len();
        }
        self.set_scroll_to_line(self.search_matches[self.current_match]);
    }

    fn cycle_link(&mut self, reverse: bool) {
        if self.doc.rendered.links.is_empty() {
            self.selected_link = None;
            return;
        }

        let len = self.doc.rendered.links.len();
        let idx = self.selected_link.unwrap_or(0);
        let next = if reverse {
            if idx == 0 {
                len - 1
            } else {
                idx - 1
            }
        } else {
            (idx + 1) % len
        };
        self.selected_link = Some(next);
        if let Some(line) = self.selected_link_line() {
            self.set_scroll_to_line(line);
        }
    }

    fn set_doc(&mut self, load: LoadResult, preserve_scroll: bool) {
        let old_scroll = self.scroll;
        let rendered = render_markdown(&load.source, &self.syntax_set, &self.theme);
        self.doc = LoadedDocument {
            path: load.path,
            rendered,
        };

        self.selected_link = if self.doc.rendered.links.is_empty() {
            None
        } else {
            Some(0)
        };

        if preserve_scroll {
            self.scroll = old_scroll;
        } else {
            self.scroll = 0;
        }

        self.reset_snapshots_from_current_doc();
        self.update_search_matches();
        self.clamp_scroll();
        self.sync_toc_selected_with_scroll();
    }

    fn reload_current(&mut self) -> Result<()> {
        let Some(path) = self.doc.path.clone() else {
            return Ok(());
        };

        let load = LoadResult {
            source: fs::read_to_string(&path)
                .with_context(|| format!("Failed to reload {}", path.display()))?,
            path: Some(path.clone()),
        };

        let rendered = render_markdown(&load.source, &self.syntax_set, &self.theme);
        self.doc.path = load.path;
        let was_live = self.is_live_mode();

        if self.push_watch_snapshot(rendered) {
            if was_live {
                if let Some(snapshot) = self.snapshots.back() {
                    if snapshot.diff.overflow {
                        self.status = format!(
                            "Reloaded {} -> r{:03} (+{}/-{}, fallback diff)",
                            path.display(),
                            snapshot.revision,
                            snapshot.diff.added,
                            snapshot.diff.removed
                        );
                    } else {
                        self.status = format!(
                            "Reloaded {} -> r{:03} (+{}/-{}, sections:{})",
                            path.display(),
                            snapshot.revision,
                            snapshot.diff.added,
                            snapshot.diff.removed,
                            snapshot.diff.section_deltas.len()
                        );
                    }
                } else {
                    self.status = format!("Reloaded {}", path.display());
                }
            } else {
                let latest_rev = self
                    .snapshots
                    .back()
                    .map(|snapshot| snapshot.revision)
                    .unwrap_or(0);
                let behind = self
                    .latest_snapshot_index()
                    .saturating_sub(self.active_snapshot);
                self.status = format!(
                    "LIVE advanced to r{:03}; viewing historical snapshot ({behind} behind)",
                    latest_rev
                );
            }
        } else {
            self.status = format!("Reloaded {} (no text changes)", path.display());
        }

        self.ensure_watcher()?;
        Ok(())
    }

    fn ensure_watcher(&mut self) -> Result<()> {
        if !self.cli.watch {
            self.watcher = None;
            return Ok(());
        }

        let Some(path) = self.doc.path.clone() else {
            self.watcher = None;
            return Ok(());
        };

        let (tx, rx) = mpsc::channel();
        let mut watcher = RecommendedWatcher::new(
            move |res| {
                let _ = tx.send(res);
            },
            Config::default(),
        )?;

        watcher.watch(&path, RecursiveMode::NonRecursive)?;
        self.watcher = Some(FileWatcher {
            _watcher: watcher,
            rx,
        });
        Ok(())
    }

    fn poll_watch(&mut self) {
        if let Some(watcher) = self.watcher.as_mut() {
            while let Ok(event) = watcher.rx.try_recv() {
                if event.is_ok() {
                    self.watch_requested = true;
                }
            }
        }
    }

    fn open_selected_link(&mut self, force_external: bool) -> Result<()> {
        let Some(link_idx) = self.selected_link else {
            self.status = "No link selected".to_string();
            return Ok(());
        };

        let link = if let Some(link) = self.doc.rendered.links.get(link_idx) {
            link.clone()
        } else {
            self.status = "Invalid link selection".to_string();
            return Ok(());
        };

        let action = classify_link(&link.target, self.doc.path.as_deref());

        match (force_external, action) {
            (_, LinkAction::Anchor(anchor)) => {
                self.status = format!("Anchor links not yet implemented: {anchor}");
            }
            (false, LinkAction::InternalMarkdown(path)) => {
                let canonical = fs::canonicalize(&path).unwrap_or(path.clone());
                if let Some(current_path) = self.doc.path.clone() {
                    self.backstack.push(HistoryEntry {
                        path: current_path,
                        scroll: self.scroll,
                    });
                }
                let source = fs::read_to_string(&canonical)
                    .with_context(|| format!("Failed to open {}", canonical.display()))?;
                self.set_doc(
                    LoadResult {
                        path: Some(canonical.clone()),
                        source,
                    },
                    false,
                );
                self.ensure_watcher()?;
                self.status = format!("Opened {}", canonical.display());
            }
            (true, LinkAction::InternalMarkdown(path)) => {
                system_open(&path).with_context(|| format!("Failed to open {}", path.display()))?;
                self.status = format!("Opened {}", path.display());
            }
            (_, LinkAction::ExternalUrl(url)) => {
                system_open(&url).with_context(|| format!("Failed to open {url}"))?;
                self.status = format!("Opened {url}");
            }
            (_, LinkAction::ExternalPath(path)) => {
                system_open(&path).with_context(|| format!("Failed to open {}", path.display()))?;
                self.status = format!("Opened {}", path.display());
            }
            (_, LinkAction::Unknown(raw)) => {
                system_open(&raw).with_context(|| format!("Failed to open {raw}"))?;
                self.status = format!("Opened {raw}");
            }
        }

        Ok(())
    }

    fn go_back(&mut self) -> Result<()> {
        let Some(entry) = self.backstack.pop() else {
            self.status = "Backstack is empty".to_string();
            return Ok(());
        };

        let source = fs::read_to_string(&entry.path)
            .with_context(|| format!("Failed to open {}", entry.path.display()))?;
        self.set_doc(
            LoadResult {
                path: Some(entry.path.clone()),
                source,
            },
            false,
        );
        self.set_scroll_and_sync(entry.scroll);
        self.ensure_watcher()?;
        self.status = format!("Returned to {}", entry.path.display());
        Ok(())
    }

    fn draw(&mut self, frame: &mut ratatui::Frame<'_>) {
        let root = inset_rect(frame.size(), 1, 0);
        let max_dock_height = root.height.saturating_sub(3);
        let (body, timeline_area, status) = if self.cli.watch
            && self.timeline_open
            && max_dock_height >= TIMELINE_MIN_HEIGHT
            && root.height >= 5
        {
            let dock_height = self
                .timeline_height
                .min(max_dock_height)
                .max(TIMELINE_MIN_HEIGHT);
            let chunks = Layout::vertical([
                Constraint::Min(1),
                Constraint::Length(dock_height),
                Constraint::Length(1),
            ])
            .split(root);
            (chunks[0], Some(chunks[1]), inset_rect(chunks[2], 1, 0))
        } else {
            let chunks = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(root);
            (chunks[0], None, inset_rect(chunks[1], 1, 0))
        };

        let content_area = if self.toc_open {
            let widths = [
                Constraint::Length(body.width.saturating_div(3).max(24)),
                Constraint::Length(1),
                Constraint::Min(1),
            ];
            let cols = Layout::horizontal(widths).split(body);
            self.draw_toc(frame, cols[0]);
            cols[2]
        } else {
            body
        };

        self.viewport_height = content_area.height.saturating_sub(1).max(1);
        self.clamp_scroll();
        self.draw_content(frame, content_area);
        if let Some(area) = timeline_area {
            self.draw_timeline(frame, area);
        }
        self.draw_status(frame, status);
    }

    fn draw_toc(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let selected = self
            .toc_selected
            .min(self.doc.rendered.toc.len().saturating_sub(1));
        let (active_diff, freshness) = if let Some(snapshot) = self.current_snapshot() {
            (
                Some(&snapshot.diff),
                change_freshness(snapshot.created_instant),
            )
        } else {
            (None, None)
        };

        let items: Vec<ListItem> = self
            .doc
            .rendered
            .toc
            .iter()
            .enumerate()
            .map(|(idx, entry)| {
                let indent = "  ".repeat(entry.level.saturating_sub(1) as usize);
                let row_style = if idx == selected {
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                let section_delta = active_diff.and_then(|diff| diff.section_deltas.get(&idx));
                let mut title = format!("{indent}{}", entry.title);
                if self.timeline_open {
                    if let Some(delta) = section_delta {
                        title.push_str(&format!(" (+{}/-{})", delta.added, delta.removed));
                    }
                }
                let change_marker = if section_delta.is_some() && freshness.is_some() {
                    let marker_style = match freshness {
                        Some(ChangeFreshness::Bright) => Style::default()
                            .fg(Color::LightRed)
                            .add_modifier(Modifier::BOLD),
                        Some(ChangeFreshness::Dim) => Style::default().fg(Color::DarkGray),
                        None => Style::default(),
                    };
                    Span::styled("● ", marker_style)
                } else {
                    Span::raw("  ")
                };

                let line = Line::from(vec![
                    Span::styled(if idx == selected { "> " } else { "  " }, row_style),
                    change_marker,
                    Span::styled(title, row_style),
                ]);
                ListItem::new(line)
            })
            .collect();

        let toc = if items.is_empty() {
            List::new(vec![ListItem::new(Line::raw("  (no h1-h3 headings)"))])
        } else {
            List::new(items)
        }
        .block(
            Block::default()
                .title(" TOC ")
                .borders(Borders::TOP)
                .border_style(Style::default().fg(Color::DarkGray))
                .padding(Padding::new(1, 1, 0, 0)),
        );

        frame.render_widget(toc, area);
    }

    fn draw_timeline(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        if self.snapshots.len() <= 1 {
            let empty = Paragraph::new(" No prior revisions yet")
                .block(
                    Block::default()
                        .title(" Timeline ")
                        .borders(Borders::TOP)
                        .border_style(Style::default().fg(Color::DarkGray))
                        .padding(Padding::new(1, 1, 0, 0)),
                )
                .style(Style::default().fg(Color::Gray));
            frame.render_widget(empty, area);
            return;
        }

        let latest = self.latest_snapshot_index();
        let items: Vec<ListItem> = self
            .snapshots
            .iter()
            .enumerate()
            .rev()
            .map(|(idx, snapshot)| {
                let top = snapshot
                    .diff
                    .top_section
                    .as_ref()
                    .map(|value| truncate_label(value, 32))
                    .unwrap_or_else(|| "-".to_string());
                let row = format!(
                    "r{:03}  {}  +{}/-{}  h:{}  top:{}{}",
                    snapshot.revision,
                    format_clock_hms(snapshot.created_at),
                    snapshot.diff.added,
                    snapshot.diff.removed,
                    snapshot.diff.section_deltas.len(),
                    top,
                    if snapshot.diff.overflow {
                        "  (fallback)"
                    } else {
                        ""
                    }
                );
                let line = if idx == self.active_snapshot {
                    Line::styled(
                        row,
                        Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD),
                    )
                } else if idx == latest {
                    Line::styled(row, Style::default().fg(Color::Cyan))
                } else {
                    Line::raw(row)
                };
                ListItem::new(line)
            })
            .collect();

        let list = List::new(items).block(
            Block::default()
                .title(" Timeline ")
                .borders(Borders::TOP)
                .border_style(Style::default().fg(Color::DarkGray))
                .padding(Padding::new(1, 1, 0, 0)),
        );

        frame.render_widget(list, area);
    }

    fn draw_content(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let selected_link_line = self.selected_link_line();
        let total_lines = self.doc.rendered.lines.len();
        let mut changed_lines = vec![false; total_lines];
        let mut hunk_anchors = vec![false; total_lines];
        let freshness = self
            .current_snapshot()
            .and_then(|snapshot| change_freshness(snapshot.created_instant));

        if let Some(snapshot) = self.current_snapshot() {
            for hunk in &snapshot.diff.hunks {
                if total_lines == 0 {
                    continue;
                }
                let anchor = hunk_anchor_line(hunk, total_lines);
                hunk_anchors[anchor] = true;

                if freshness.is_some() {
                    if hunk.end_line > hunk.start_line {
                        for line in hunk.start_line.min(total_lines)..hunk.end_line.min(total_lines)
                        {
                            changed_lines[line] = true;
                        }
                    } else {
                        changed_lines[anchor] = true;
                    }
                }
            }
        }

        let lines: Vec<Line> = self
            .doc
            .rendered
            .lines
            .iter()
            .enumerate()
            .map(|(idx, line)| {
                let is_match = self.search_matches.binary_search(&idx).is_ok();
                let is_selected_link_line = selected_link_line == Some(idx);
                let is_changed = changed_lines.get(idx).copied().unwrap_or(false);
                let is_hunk_anchor = hunk_anchors.get(idx).copied().unwrap_or(false);

                let base_marker_style = match freshness {
                    Some(ChangeFreshness::Bright) => Style::default()
                        .fg(Color::LightRed)
                        .add_modifier(Modifier::BOLD),
                    Some(ChangeFreshness::Dim) => Style::default().fg(Color::Gray),
                    None => Style::default().fg(Color::LightBlue),
                };
                let marker_span = if is_hunk_anchor {
                    Span::styled("▌ ", base_marker_style)
                } else {
                    Span::styled("  ", Style::default())
                };

                let mut spans = vec![marker_span];
                if line.segments.is_empty() {
                    spans.push(Span::raw(""));
                } else {
                    spans.extend(line.segments.iter().map(|segment| {
                        let mut style = segment.style;
                        if is_changed {
                            style = match freshness {
                                Some(ChangeFreshness::Bright) => style.bg(Color::Rgb(70, 35, 0)),
                                Some(ChangeFreshness::Dim) => style.bg(Color::Rgb(36, 36, 36)),
                                None => style,
                            };
                        }
                        if is_match {
                            style = style.bg(Color::Rgb(40, 40, 40));
                        }
                        if is_selected_link_line {
                            style = style.bg(Color::Blue).fg(Color::White);
                        }
                        Span::styled(segment.text.clone(), style)
                    }));
                }
                Line::from(spans)
            })
            .collect();

        let paragraph = Paragraph::new(Text::from(lines))
            .block(
                Block::default()
                    .title(" catmd ")
                    .borders(Borders::TOP)
                    .border_style(Style::default().fg(Color::DarkGray))
                    .padding(Padding::new(1, 1, 0, 0)),
            )
            .scroll((self.scroll, 0))
            .wrap(Wrap { trim: false });

        frame.render_widget(paragraph, area);
    }

    fn draw_status(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let path = self
            .doc
            .path
            .as_ref()
            .map_or_else(|| "<stdin>".to_string(), |p| p.display().to_string());

        let link_hint = if let Some(idx) = self.selected_link {
            self.doc.rendered.links.get(idx).map_or_else(
                || "link: none".to_string(),
                |link| {
                    format!(
                        "link[{}/{}]: {}",
                        idx + 1,
                        self.doc.rendered.links.len(),
                        link.label
                    )
                },
            )
        } else {
            "link: none".to_string()
        };

        let search_hint = if self.search_mode {
            format!(" /{}", self.search_query)
        } else if self.search_query.is_empty() {
            String::new()
        } else {
            format!(
                " search='{}' {}/{}",
                self.search_query,
                if self.search_matches.is_empty() {
                    0
                } else {
                    self.current_match + 1
                },
                self.search_matches.len()
            )
        };

        let mode_hint = if self.cli.watch {
            if let Some(snapshot) = self.current_snapshot() {
                let behind = self
                    .latest_snapshot_index()
                    .saturating_sub(self.active_snapshot);
                if behind == 0 {
                    format!(
                        "LIVE r{:03} | +{}/-{} | sections:{} | watch:on",
                        snapshot.revision,
                        snapshot.diff.added,
                        snapshot.diff.removed,
                        snapshot.diff.section_deltas.len()
                    )
                } else {
                    format!(
                        "HISTORY r{:03} ({behind} behind LIVE) | +{}/-{} | hunks:{}",
                        snapshot.revision,
                        snapshot.diff.added,
                        snapshot.diff.removed,
                        snapshot.diff.hunks.len()
                    )
                }
            } else {
                "watch:on".to_string()
            }
        } else {
            String::new()
        };

        let mut parts = Vec::new();
        if !mode_hint.is_empty() {
            parts.push(mode_hint);
        }
        parts.push(path);
        parts.push(format!("{link_hint}{search_hint}"));
        if !self.status.is_empty() {
            parts.push(self.status.clone());
        }
        let status_text = if parts.is_empty() {
            String::new()
        } else {
            parts.join(" | ")
        };

        frame.render_widget(
            Paragraph::new(format!(" {status_text}")).style(Style::default().fg(Color::Gray)),
            area,
        );
    }

    fn handle_search_input(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Enter => {
                self.search_mode = false;
            }
            KeyCode::Backspace => {
                self.search_query.pop();
                self.current_match = 0;
                self.update_search_matches();
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.search_query.push(c);
                self.current_match = 0;
                self.update_search_matches();
            }
            _ => {}
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> Result<bool> {
        if self.search_mode {
            self.handle_search_input(key);
            return Ok(false);
        }

        match key.code {
            KeyCode::Char('q') => return Ok(true),
            KeyCode::Char('v') => {
                self.toggle_timeline();
            }
            KeyCode::Char('h') | KeyCode::Left => {
                self.move_revision_relative(true);
            }
            KeyCode::Char('l') | KeyCode::Right => {
                self.move_revision_relative(false);
            }
            KeyCode::Char('L') => {
                self.jump_to_live_revision();
            }
            KeyCode::Char('(') => {
                self.jump_hunk_relative(true);
            }
            KeyCode::Char(')') => {
                self.jump_hunk_relative(false);
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if self.toc_open {
                    self.move_toc_selection(false);
                } else {
                    self.set_scroll_and_sync(self.scroll.saturating_add(1));
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if self.toc_open {
                    self.move_toc_selection(true);
                } else {
                    self.set_scroll_and_sync(self.scroll.saturating_sub(1));
                }
            }
            KeyCode::Char('g') => {
                self.set_scroll_and_sync(0);
            }
            KeyCode::Char('G') => {
                self.set_scroll_and_sync(self.max_scroll());
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let delta = self.viewport_height.saturating_div(2).max(1);
                self.set_scroll_and_sync(self.scroll.saturating_add(delta));
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let delta = self.viewport_height.saturating_div(2).max(1);
                self.set_scroll_and_sync(self.scroll.saturating_sub(delta));
            }
            KeyCode::Char('t') => {
                self.toc_open = !self.toc_open;
                if self.toc_open {
                    self.sync_toc_selected_with_scroll();
                }
            }
            KeyCode::Tab => {
                self.cycle_link(false);
            }
            KeyCode::BackTab => {
                self.cycle_link(true);
            }
            KeyCode::Enter => {
                if self.toc_open {
                    self.jump_to_toc_selected();
                } else {
                    self.open_selected_link(false)?;
                }
            }
            KeyCode::Char('o') => {
                self.open_selected_link(true)?;
            }
            KeyCode::Char(']') => {
                self.jump_heading_relative(false);
            }
            KeyCode::Char('[') => {
                self.jump_heading_relative(true);
            }
            KeyCode::Backspace => {
                self.go_back()?;
            }
            KeyCode::Char('/') => {
                self.search_mode = true;
                self.search_query.clear();
                self.search_matches.clear();
                self.current_match = 0;
            }
            KeyCode::Char('n') => {
                self.jump_to_next_match(false);
            }
            KeyCode::Char('N') => {
                self.jump_to_next_match(true);
            }
            _ => {}
        }

        Ok(false)
    }
}

struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode()?;
        io::stdout().execute(EnterAlternateScreen)?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}

fn run_interactive(mut app: App) -> Result<()> {
    let _guard = TerminalGuard::enter()?;
    app.ensure_watcher()?;

    let stdout = io::stdout();
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    loop {
        terminal.draw(|frame| app.draw(frame))?;

        if app.watch_requested {
            if let Err(err) = app.reload_current() {
                app.status = format!("Reload failed: {err:#}");
            }
            app.watch_requested = false;
        }

        app.poll_watch();

        if event::poll(Duration::from_millis(120))? {
            match event::read()? {
                CEvent::Key(key) if key.kind == KeyEventKind::Press => {
                    if app.handle_key(key)? {
                        break;
                    }
                }
                _ => {}
            }
        }
    }

    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    if cli.interactive && cli.plain {
        return Err(anyhow!("--interactive and --plain cannot be used together"));
    }

    let input = detect_input(&cli)?;
    if cli.watch && matches!(input, InputSource::Stdin) {
        return Err(anyhow!("--watch requires file input"));
    }

    let interactive = if cli.interactive {
        true
    } else if cli.plain {
        false
    } else {
        default_interactive(&input)
    };

    let load = read_input(&input)?;

    let syntax_set = SyntaxSet::load_defaults_newlines();
    let theme_set = ThemeSet::load_defaults();
    let theme = resolve_theme(&theme_set);

    let rendered = render_markdown(&load.source, &syntax_set, &theme);

    if !interactive {
        print!("{}", plain_render(&rendered));
        return Ok(());
    }

    let app = App::new(cli, load, rendered, syntax_set, theme);
    run_interactive(app)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_doc(lines: &[&str], toc: &[(u8, &str, usize)]) -> RenderedDocument {
        RenderedDocument {
            lines: lines
                .iter()
                .map(|line| RenderedLine {
                    segments: Vec::new(),
                    plain: (*line).to_string(),
                })
                .collect(),
            toc: toc
                .iter()
                .map(|(level, title, line)| TocEntry {
                    level: *level,
                    title: (*title).to_string(),
                    line: *line,
                })
                .collect(),
            links: Vec::new(),
        }
    }

    #[test]
    fn compute_line_diff_detects_insert_hunk() {
        let old_lines = vec!["a", "b", "c"];
        let new_lines = vec!["a", "b", "x", "c"];
        let diff = compute_line_diff(&old_lines, &new_lines, 1_000);

        assert_eq!(diff.added, 1);
        assert_eq!(diff.removed, 0);
        assert_eq!(diff.hunks.len(), 1);
        assert_eq!(diff.hunks[0].start_line, 2);
        assert_eq!(diff.hunks[0].end_line, 3);
    }

    #[test]
    fn compute_line_diff_detects_replacement_hunk() {
        let old_lines = vec!["a", "b", "c"];
        let new_lines = vec!["a", "z", "c"];
        let diff = compute_line_diff(&old_lines, &new_lines, 1_000);

        assert_eq!(diff.added, 1);
        assert_eq!(diff.removed, 1);
        assert_eq!(diff.hunks.len(), 1);
        assert_eq!(diff.hunks[0].start_line, 1);
        assert_eq!(diff.hunks[0].end_line, 2);
    }

    #[test]
    fn build_snapshot_diff_maps_changed_section() {
        let old_doc = test_doc(
            &["# Intro", "", "hello", "## Details", "", "world"],
            &[(1, "Intro", 0), (2, "Details", 3)],
        );
        let new_doc = test_doc(
            &["# Intro", "", "hello", "## Details", "", "planet"],
            &[(1, "Intro", 0), (2, "Details", 3)],
        );
        let diff = build_snapshot_diff(&old_doc, &new_doc);

        assert_eq!(diff.section_deltas.len(), 1);
        assert!(diff.section_deltas.contains_key(&1));
        assert_eq!(diff.top_section.as_deref(), Some("Details"));
    }

    #[test]
    fn compute_line_diff_falls_back_for_large_matrix() {
        let old_lines: Vec<String> = (0..60).map(|idx| format!("a{idx}")).collect();
        let new_lines: Vec<String> = (0..60).map(|idx| format!("b{idx}")).collect();
        let old_refs: Vec<&str> = old_lines.iter().map(|line| line.as_str()).collect();
        let new_refs: Vec<&str> = new_lines.iter().map(|line| line.as_str()).collect();

        let diff = compute_line_diff(&old_refs, &new_refs, 100);
        assert!(diff.overflow);
        assert_eq!(diff.hunks.len(), 1);
        assert_eq!(diff.added, 60);
        assert_eq!(diff.removed, 60);
    }
}
