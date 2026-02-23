use std::collections::VecDeque;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use crossterm::event::{self, Event as CEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::{execute, ExecutableCommand};
use notify::{Config, Event, RecommendedWatcher, RecursiveMode, Watcher};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout};
use ratatui::prelude::{Color, Modifier, Rect, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::block::Padding;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use ratatui::Terminal;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::SyntaxSet;

mod agent;
mod diff;
mod input;
mod links;
mod markdown;

use agent::{extract_agent_tasks, AgentTask, AgentTaskState};
use diff::{
    build_snapshot_diff, change_freshness, format_clock_hms, hunk_anchor_line, truncate_label,
    ChangeFreshness, SnapshotDiff, WatchSnapshot,
};
use input::{default_interactive, detect_input, read_input, Cli, InputSource, LoadResult};
use links::{classify_link, system_open, LinkAction};
use markdown::{plain_render, render_markdown, RenderedDocument};

#[cfg(test)]
use diff::compute_line_diff;
#[cfg(test)]
use markdown::{RenderedLine, TocEntry};

const NO_TOC_HEADINGS_STATUS: &str = "No headings in TOC";
const TIMELINE_DEFAULT_HEIGHT: u16 = 6;
const TIMELINE_MIN_HEIGHT: u16 = 3;
const NO_AGENT_TASKS_STATUS: &str = "No agent tasks found";
const NO_OPEN_AGENT_TASKS_STATUS: &str = "All agent tasks complete";

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

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::vertical([
        Constraint::Percentage((100_u16.saturating_sub(percent_y)) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100_u16.saturating_sub(percent_y)) / 2),
    ])
    .split(area);
    Layout::horizontal([
        Constraint::Percentage((100_u16.saturating_sub(percent_x)) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100_u16.saturating_sub(percent_x)) / 2),
    ])
    .split(vertical[1])[1]
}

fn usize_to_u16_saturating(value: usize) -> u16 {
    match u16::try_from(value) {
        Ok(v) => v,
        Err(_) => u16::MAX,
    }
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

struct FileWatcher {
    _watcher: RecommendedWatcher,
    rx: Receiver<notify::Result<Event>>,
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
    agent_inbox_open: bool,
    agent_selected: usize,
    help_open: bool,
    timeline_open: bool,
    timeline_height: u16,

    selected_link: Option<usize>,
    backstack: Vec<HistoryEntry>,

    search_mode: bool,
    search_query: String,
    search_matches: Vec<usize>,
    current_match: usize,
    quick_task_mode: bool,
    quick_task_input: String,

    agent_tasks: Vec<AgentTask>,
    open_agent_tasks: Vec<usize>,

    status: String,

    watcher: Option<FileWatcher>,
    watch_requested: bool,
}

impl App {
    fn first_link_selection(rendered: &RenderedDocument) -> Option<usize> {
        if rendered.links.is_empty() {
            None
        } else {
            Some(0)
        }
    }

    fn reset_selected_link(&mut self) {
        self.selected_link = Self::first_link_selection(&self.doc.rendered);
    }

    fn require_watch_mode(&mut self, status: &str) -> bool {
        if self.cli.watch {
            true
        } else {
            self.status = status.to_string();
            false
        }
    }

    fn new(
        cli: Cli,
        load: LoadResult,
        rendered: RenderedDocument,
        syntax_set: SyntaxSet,
        theme: Theme,
    ) -> Self {
        let selected_link = Self::first_link_selection(&rendered);
        let agent_tasks = extract_agent_tasks(&rendered);
        let open_agent_tasks = agent_tasks
            .iter()
            .enumerate()
            .filter_map(|(idx, task)| task.is_open().then_some(idx))
            .collect();
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
            agent_inbox_open: false,
            agent_selected: 0,
            help_open: false,
            timeline_open: false,
            timeline_height: TIMELINE_DEFAULT_HEIGHT,
            selected_link,
            backstack: Vec::new(),
            search_mode: false,
            search_query: String::new(),
            search_matches: Vec::new(),
            current_match: 0,
            quick_task_mode: false,
            quick_task_input: String::new(),
            agent_tasks,
            open_agent_tasks,
            status: String::new(),
            watcher: None,
            watch_requested: false,
        }
    }

    fn refresh_agent_tasks(&mut self) {
        let selected_line = self
            .open_agent_tasks
            .get(self.agent_selected)
            .and_then(|idx| self.agent_tasks.get(*idx))
            .map(|task| task.line);

        self.agent_tasks = extract_agent_tasks(&self.doc.rendered);
        self.open_agent_tasks = self
            .agent_tasks
            .iter()
            .enumerate()
            .filter_map(|(idx, task)| task.is_open().then_some(idx))
            .collect();

        if self.open_agent_tasks.is_empty() {
            self.agent_selected = 0;
            return;
        }

        if let Some(line) = selected_line {
            if let Some(position) = self
                .open_agent_tasks
                .iter()
                .position(|idx| self.agent_tasks[*idx].line == line)
            {
                self.agent_selected = position;
                return;
            }
        }

        self.agent_selected = self
            .agent_selected
            .min(self.open_agent_tasks.len().saturating_sub(1));
    }

    fn selected_open_agent_task(&self) -> Option<&AgentTask> {
        self.open_agent_tasks
            .get(self.agent_selected)
            .and_then(|idx| self.agent_tasks.get(*idx))
    }

    fn selected_open_agent_line(&self) -> Option<usize> {
        self.selected_open_agent_task().map(|task| task.line)
    }

    fn status_for_empty_open_tasks(&mut self) {
        self.status = if self.agent_tasks.is_empty() {
            NO_AGENT_TASKS_STATUS.to_string()
        } else {
            NO_OPEN_AGENT_TASKS_STATUS.to_string()
        };
    }

    fn sync_agent_selected_with_scroll(&mut self) {
        if self.open_agent_tasks.is_empty() {
            self.agent_selected = 0;
            return;
        }

        let line = usize::from(self.scroll);
        self.agent_selected = self
            .open_agent_tasks
            .iter()
            .enumerate()
            .rfind(|(_, idx)| self.agent_tasks[**idx].line <= line)
            .map_or(0, |(position, _)| position);
    }

    fn toggle_toc(&mut self) {
        self.toc_open = !self.toc_open;
        if self.toc_open {
            self.agent_inbox_open = false;
            self.sync_toc_selected_with_scroll();
        }
    }

    fn toggle_agent_inbox(&mut self) {
        self.agent_inbox_open = !self.agent_inbox_open;
        if self.agent_inbox_open {
            self.toc_open = false;
            self.sync_agent_selected_with_scroll();
        }
    }

    fn move_agent_selection(&mut self, reverse: bool) {
        let len = self.open_agent_tasks.len();
        if len == 0 {
            self.status_for_empty_open_tasks();
            return;
        }

        if reverse {
            self.agent_selected = self.agent_selected.saturating_sub(1);
        } else {
            self.agent_selected = (self.agent_selected + 1).min(len.saturating_sub(1));
        }
    }

    fn jump_to_open_agent_index(&mut self, index: usize) {
        let len = self.open_agent_tasks.len();
        if len == 0 {
            self.status_for_empty_open_tasks();
            return;
        }

        self.agent_selected = index.min(len.saturating_sub(1));
        let Some(task) = self.selected_open_agent_task().cloned() else {
            self.status_for_empty_open_tasks();
            return;
        };

        self.set_scroll_to_line(task.line);
        self.status = format!(
            "Agent task {}/{}: {}",
            self.agent_selected + 1,
            len,
            truncate_label(&task.text, 48)
        );
    }

    fn jump_to_selected_agent_task(&mut self) {
        self.jump_to_open_agent_index(self.agent_selected);
    }

    fn jump_agent_task_relative(&mut self, reverse: bool) {
        let len = self.open_agent_tasks.len();
        if len == 0 {
            self.status_for_empty_open_tasks();
            return;
        }

        let line = usize::from(self.scroll);
        let target = if reverse {
            self.open_agent_tasks
                .iter()
                .enumerate()
                .rev()
                .find(|(_, idx)| self.agent_tasks[**idx].line < line)
                .map_or(len.saturating_sub(1), |(position, _)| position)
        } else {
            self.open_agent_tasks
                .iter()
                .enumerate()
                .find(|(_, idx)| self.agent_tasks[**idx].line > line)
                .map_or(0, |(position, _)| position)
        };

        self.jump_to_open_agent_index(target);
    }

    fn begin_quick_task_capture(&mut self) {
        if self.doc.path.is_none() {
            self.status = "Quick task capture requires file input".to_string();
            return;
        }
        self.quick_task_mode = true;
        self.quick_task_input.clear();
        self.status = "New @agent task: type text, Enter to save, Esc to cancel".to_string();
    }

    fn submit_quick_task_capture(&mut self) {
        let task_text = self.quick_task_input.trim().to_string();
        if task_text.is_empty() {
            self.status = "Agent task cannot be empty".to_string();
            return;
        }

        let Some(path) = self.doc.path.clone() else {
            self.quick_task_mode = false;
            self.quick_task_input.clear();
            self.status = "Quick task capture requires file input".to_string();
            return;
        };

        let result: Result<()> = (|| {
            let mut source = fs::read_to_string(&path)
                .with_context(|| format!("Failed to read {}", path.display()))?;
            let task_line = format!("- [ ] @agent {task_text}");
            if source.is_empty() {
                source.push_str(&task_line);
                source.push('\n');
            } else {
                if !source.ends_with('\n') {
                    source.push('\n');
                }
                source.push_str(&task_line);
                source.push('\n');
            }

            fs::write(&path, source).with_context(|| format!("Failed to write {}", path.display()))
        })();

        self.quick_task_mode = false;
        self.quick_task_input.clear();

        if let Err(err) = result {
            self.status = format!("Failed to add agent task: {err:#}");
            return;
        }

        if let Err(err) = self.reload_current() {
            self.status = format!("Reload failed after adding task: {err:#}");
            return;
        }
        self.watch_requested = false;
        self.status = format!("Added agent task: {}", truncate_label(&task_text, 48));
    }

    fn handle_quick_task_input(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.quick_task_mode = false;
                self.quick_task_input.clear();
                self.status = "Canceled new @agent task".to_string();
            }
            KeyCode::Enter => {
                self.submit_quick_task_capture();
            }
            KeyCode::Backspace => {
                self.quick_task_input.pop();
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.quick_task_input.push(c);
            }
            _ => {}
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
        self.reset_selected_link();
        self.refresh_agent_tasks();

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
        self.sync_agent_selected_with_scroll();
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
        if !self.require_watch_mode("Timeline is available only in --watch mode") {
            return;
        }
        self.timeline_open = !self.timeline_open;
    }

    fn move_revision_relative(&mut self, older: bool) {
        if !self.require_watch_mode("Revision navigation is available only in --watch mode") {
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
        if !self.require_watch_mode("Jump-to-live is available only in --watch mode") {
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
        self.sync_agent_selected_with_scroll();
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

        self.reset_selected_link();

        if preserve_scroll {
            self.scroll = old_scroll;
        } else {
            self.scroll = 0;
        }

        self.reset_snapshots_from_current_doc();
        self.refresh_agent_tasks();
        self.update_search_matches();
        self.clamp_scroll();
        self.sync_toc_selected_with_scroll();
        self.sync_agent_selected_with_scroll();
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

        let content_area = if self.toc_open || self.agent_inbox_open {
            let widths = [
                Constraint::Length(body.width.saturating_div(3).max(24)),
                Constraint::Length(1),
                Constraint::Min(1),
            ];
            let cols = Layout::horizontal(widths).split(body);
            if self.agent_inbox_open {
                self.draw_agent_inbox(frame, cols[0]);
            } else {
                self.draw_toc(frame, cols[0]);
            }
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
        if self.help_open {
            self.draw_shortcuts_help(frame);
        }
        if self.quick_task_mode {
            self.draw_quick_task_capture(frame);
        }
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

    fn draw_agent_inbox(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let block = Block::default()
            .title(" Agent Inbox ")
            .borders(Borders::TOP)
            .border_style(Style::default().fg(Color::DarkGray))
            .padding(Padding::new(1, 1, 0, 0));

        if self.agent_tasks.is_empty() {
            frame.render_widget(
                Paragraph::new(format!(" {NO_AGENT_TASKS_STATUS}"))
                    .style(Style::default().fg(Color::Gray))
                    .block(block),
                area,
            );
            return;
        }

        if self.open_agent_tasks.is_empty() {
            frame.render_widget(
                Paragraph::new(format!(" {NO_OPEN_AGENT_TASKS_STATUS}"))
                    .style(Style::default().fg(Color::Gray))
                    .block(block),
                area,
            );
            return;
        }

        let selected = self
            .agent_selected
            .min(self.open_agent_tasks.len().saturating_sub(1));
        let items: Vec<ListItem> = self
            .open_agent_tasks
            .iter()
            .enumerate()
            .filter_map(|(position, task_index)| {
                let task = self.agent_tasks.get(*task_index)?;
                let row = format!("{:>4}  {}", task.line + 1, truncate_label(&task.text, 44));
                let line = if position == selected {
                    Line::styled(
                        row,
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    )
                } else {
                    Line::raw(row)
                };
                Some(ListItem::new(line))
            })
            .collect();

        let list = List::new(items).block(block);
        frame.render_widget(list, area);
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
        let selected_open_agent_line = self.selected_open_agent_line();
        let total_lines = self.doc.rendered.lines.len();
        let mut changed_lines = vec![false; total_lines];
        let mut hunk_anchors = vec![false; total_lines];
        let mut agent_states = vec![None; total_lines];
        let freshness = self
            .current_snapshot()
            .and_then(|snapshot| change_freshness(snapshot.created_instant));

        for task in &self.agent_tasks {
            if task.line < total_lines {
                agent_states[task.line] = Some(task.state);
            }
        }

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
                let is_selected_agent_line = selected_open_agent_line == Some(idx);
                let is_changed = changed_lines.get(idx).copied().unwrap_or(false);
                let is_hunk_anchor = hunk_anchors.get(idx).copied().unwrap_or(false);
                let agent_state = agent_states.get(idx).copied().flatten();

                let base_marker_style = match freshness {
                    Some(ChangeFreshness::Bright) => Style::default()
                        .fg(Color::LightRed)
                        .add_modifier(Modifier::BOLD),
                    Some(ChangeFreshness::Dim) => Style::default().fg(Color::Gray),
                    None => Style::default().fg(Color::LightBlue),
                };
                let marker_span = if is_hunk_anchor {
                    Span::styled("▌ ", base_marker_style)
                } else if let Some(state) = agent_state {
                    match state {
                        AgentTaskState::Open => Span::styled(
                            "@ ",
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                        AgentTaskState::Done => {
                            Span::styled("@ ", Style::default().fg(Color::DarkGray))
                        }
                    }
                } else {
                    Span::styled("  ", Style::default())
                };

                let mut spans = vec![marker_span];
                if line.segments.is_empty() {
                    spans.push(Span::raw(""));
                } else {
                    spans.extend(line.segments.iter().map(|segment| {
                        let mut style = segment.style;
                        if let Some(state) = agent_state {
                            style = match state {
                                AgentTaskState::Open => style.bg(Color::Rgb(16, 52, 44)),
                                AgentTaskState::Done => style.fg(Color::DarkGray),
                            };
                        }
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
                        if is_selected_agent_line && !is_selected_link_line {
                            style = style.add_modifier(Modifier::BOLD);
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
        let quick_task_hint = if self.quick_task_mode {
            if self.quick_task_input.is_empty() {
                " new_task='…'".to_string()
            } else {
                format!(" new_task='{}'", truncate_label(&self.quick_task_input, 48))
            }
        } else {
            String::new()
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
        let agent_hint = if self.agent_tasks.is_empty() {
            String::new()
        } else {
            format!(
                "agent: {}/{} open",
                self.open_agent_tasks.len(),
                self.agent_tasks.len()
            )
        };

        let mut parts = Vec::new();
        if !self.status.is_empty() {
            parts.push(self.status.clone());
        }
        if !mode_hint.is_empty() {
            parts.push(mode_hint);
        }
        if !agent_hint.is_empty() {
            parts.push(agent_hint);
        }
        parts.push(path);
        parts.push(format!("{link_hint}{search_hint}{quick_task_hint}"));
        let status_text = parts.join(" | ");

        frame.render_widget(
            Paragraph::new(format!(" {status_text}")).style(Style::default().fg(Color::Gray)),
            area,
        );
    }

    fn draw_shortcuts_help(&self, frame: &mut ratatui::Frame<'_>) {
        let area = centered_rect(78, 88, frame.size());
        let lines = vec![
            Line::styled(
                "General",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Line::raw("  q                Quit"),
            Line::raw("  ?                Toggle this shortcuts view"),
            Line::raw("  A or Ctrl-a      Add new @agent task (append to file)"),
            Line::raw("  j / k            Scroll down / up"),
            Line::raw("  Ctrl-d / Ctrl-u  Half-page down / up"),
            Line::raw("  g / G            Top / bottom"),
            Line::raw("  /                Search"),
            Line::raw("  n / N            Next / previous match"),
            Line::raw(""),
            Line::styled(
                "Navigation",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Line::raw("  t                Toggle TOC"),
            Line::raw("  a                Toggle Agent Inbox"),
            Line::raw("  [ / ]            Previous / next heading"),
            Line::raw("  { / }            Previous / next unresolved @agent task"),
            Line::raw("  Enter            Follow selected item/link"),
            Line::raw("  Tab / Shift-Tab  Next / previous link"),
            Line::raw("  o                Open selected link externally"),
            Line::raw("  Backspace        Go back in local markdown history"),
            Line::raw(""),
            Line::styled(
                "Watch Mode",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Line::raw("  v                Toggle timeline"),
            Line::raw("  h / l            Older / newer revision"),
            Line::raw("  L                Jump to live revision"),
            Line::raw("  ( / )            Previous / next changed hunk"),
            Line::raw(""),
            Line::styled(
                "Panels",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Line::raw("  j / k            Move selection in TOC or Agent Inbox"),
            Line::raw("  Enter            Jump to selected TOC heading or agent task"),
            Line::raw(""),
            Line::styled(
                "Press '?' (or Esc / q) to close",
                Style::default().fg(Color::Gray),
            ),
        ];

        let panel = Paragraph::new(Text::from(lines))
            .block(
                Block::default()
                    .title(" Keyboard Shortcuts ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan))
                    .padding(Padding::new(1, 1, 0, 0)),
            )
            .wrap(Wrap { trim: false });

        frame.render_widget(Clear, area);
        frame.render_widget(panel, area);
    }

    fn draw_quick_task_capture(&self, frame: &mut ratatui::Frame<'_>) {
        let area = centered_rect(74, 26, frame.size());
        let entry = if self.quick_task_input.is_empty() {
            "(type task text)".to_string()
        } else {
            self.quick_task_input.clone()
        };
        let lines = vec![
            Line::styled(
                "Append unresolved @agent task",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Line::raw(""),
            Line::styled(
                format!("> {}", truncate_label(&entry, 120)),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Line::raw(""),
            Line::styled(
                "Enter saves to file, Esc cancels",
                Style::default().fg(Color::Gray),
            ),
        ];

        let panel = Paragraph::new(Text::from(lines))
            .block(
                Block::default()
                    .title(" New Agent Task ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan))
                    .padding(Padding::new(1, 1, 0, 0)),
            )
            .wrap(Wrap { trim: false });

        frame.render_widget(Clear, area);
        frame.render_widget(panel, area);
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

        if self.quick_task_mode {
            self.handle_quick_task_input(key);
            return Ok(false);
        }

        if self.help_open {
            match key.code {
                KeyCode::Char('?') | KeyCode::Esc | KeyCode::Char('q') => {
                    self.help_open = false;
                }
                _ => {}
            }
            return Ok(false);
        }

        match key.code {
            KeyCode::Char('q') => return Ok(true),
            KeyCode::Char('?') => {
                self.help_open = true;
            }
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.begin_quick_task_capture();
            }
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.begin_quick_task_capture();
            }
            KeyCode::Char('A') => {
                self.begin_quick_task_capture();
            }
            KeyCode::Char('v') => {
                self.toggle_timeline();
            }
            KeyCode::Char('a') => {
                self.toggle_agent_inbox();
            }
            KeyCode::Char('h') | KeyCode::Left => {
                self.move_revision_relative(true);
            }
            KeyCode::Char('l') | KeyCode::Right => {
                self.move_revision_relative(false);
            }
            KeyCode::Char('{') => {
                self.jump_agent_task_relative(true);
            }
            KeyCode::Char('}') => {
                self.jump_agent_task_relative(false);
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
                if self.agent_inbox_open {
                    self.move_agent_selection(false);
                } else if self.toc_open {
                    self.move_toc_selection(false);
                } else {
                    self.set_scroll_and_sync(self.scroll.saturating_add(1));
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if self.agent_inbox_open {
                    self.move_agent_selection(true);
                } else if self.toc_open {
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
                self.toggle_toc();
            }
            KeyCode::Tab => {
                self.cycle_link(false);
            }
            KeyCode::BackTab => {
                self.cycle_link(true);
            }
            KeyCode::Enter => {
                if self.agent_inbox_open {
                    self.jump_to_selected_agent_task();
                } else if self.toc_open {
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

    let interactive = match (cli.interactive, cli.plain) {
        (true, false) => true,
        (false, true) => false,
        _ => default_interactive(&input),
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
