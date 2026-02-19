use pulldown_cmark::{
    Alignment, CodeBlockKind, Event as MdEvent, HeadingLevel, Options, Parser as MdParser, Tag,
    TagEnd,
};
use ratatui::prelude::{Color, Modifier, Style};
use syntect::easy::HighlightLines;
use syntect::highlighting::Theme;
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

const HORIZONTAL_RULE: &str = "────────────────────────────────────────────────────────────────";

#[derive(Clone, Debug)]
pub(crate) struct StyledSegment {
    pub(crate) text: String,
    pub(crate) style: Style,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct RenderedLine {
    pub(crate) segments: Vec<StyledSegment>,
    pub(crate) plain: String,
}

#[derive(Clone, Debug)]
pub(crate) struct LinkRef {
    pub(crate) label: String,
    pub(crate) target: String,
    pub(crate) line: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct TocEntry {
    pub(crate) level: u8,
    pub(crate) title: String,
    pub(crate) line: usize,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct RenderedDocument {
    pub(crate) lines: Vec<RenderedLine>,
    pub(crate) toc: Vec<TocEntry>,
    pub(crate) links: Vec<LinkRef>,
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
        let style = self
            .heading_level
            .map(heading_style)
            .unwrap_or_else(|| self.inline.style());
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
                let list = ListState {
                    ordered: start.is_some(),
                    next_index: start.unwrap_or(1),
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
                    let label = if link.text.trim().is_empty() {
                        link.target.clone()
                    } else {
                        link.text.trim().to_string()
                    };
                    let link_ref = LinkRef {
                        label,
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
        self.push_text(HORIZONTAL_RULE, Style::default().fg(Color::DarkGray));
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

        let Some((header, body_rows)) = rows.split_first() else {
            return;
        };

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

        for row in body_rows {
            let row_line = Self::format_table_row(row, &widths);
            self.push_text(&row_line, Style::default());
            self.flush_line(false);
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

pub(crate) fn render_markdown(
    source: &str,
    syntax_set: &SyntaxSet,
    theme: &Theme,
) -> RenderedDocument {
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

pub(crate) fn plain_render(doc: &RenderedDocument) -> String {
    let mut out = String::new();
    for (idx, line) in doc.lines.iter().enumerate() {
        out.push_str(&line.plain);
        if idx + 1 < doc.lines.len() {
            out.push('\n');
        }
    }
    out
}
fn heading_style(level: u8) -> Style {
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
}
