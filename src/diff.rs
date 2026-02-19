use std::collections::BTreeMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::markdown::{RenderedDocument, TocEntry};

const BRIGHT_CHANGE_WINDOW: Duration = Duration::from_secs(2);
const DIM_CHANGE_WINDOW: Duration = Duration::from_secs(15);
const DIFF_MAX_CELLS: usize = 2_000_000;

fn clamp_line(line: usize, total_lines: usize) -> usize {
    if total_lines == 0 {
        0
    } else {
        line.min(total_lines.saturating_sub(1))
    }
}

fn common_prefix_len(old_lines: &[&str], new_lines: &[&str]) -> usize {
    let mut prefix = 0usize;
    while prefix < old_lines.len()
        && prefix < new_lines.len()
        && old_lines[prefix] == new_lines[prefix]
    {
        prefix = prefix.saturating_add(1);
    }
    prefix
}

fn trim_common_suffix(old_lines: &[&str], new_lines: &[&str], prefix: usize) -> (usize, usize) {
    let mut old_end = old_lines.len();
    let mut new_end = new_lines.len();
    while old_end > prefix
        && new_end > prefix
        && old_lines[old_end.saturating_sub(1)] == new_lines[new_end.saturating_sub(1)]
    {
        old_end = old_end.saturating_sub(1);
        new_end = new_end.saturating_sub(1);
    }
    (old_end, new_end)
}

fn single_hunk_result(
    start_line: usize,
    added: usize,
    removed: usize,
    overflow: bool,
) -> LineDiffResult {
    LineDiffResult {
        added,
        removed,
        hunks: vec![DiffHunk {
            start_line,
            end_line: start_line.saturating_add(added),
            added,
            removed,
        }],
        overflow,
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SectionDelta {
    pub(crate) added: usize,
    pub(crate) removed: usize,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct DiffHunk {
    pub(crate) start_line: usize,
    pub(crate) end_line: usize,
    pub(crate) added: usize,
    pub(crate) removed: usize,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct SnapshotDiff {
    pub(crate) added: usize,
    pub(crate) removed: usize,
    pub(crate) hunks: Vec<DiffHunk>,
    pub(crate) section_deltas: BTreeMap<usize, SectionDelta>,
    pub(crate) top_section: Option<String>,
    pub(crate) overflow: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct WatchSnapshot {
    pub(crate) revision: u64,
    pub(crate) created_at: SystemTime,
    pub(crate) created_instant: Instant,
    pub(crate) rendered: RenderedDocument,
    pub(crate) diff: SnapshotDiff,
}

#[derive(Debug, Default)]
pub(crate) struct LineDiffResult {
    pub(crate) added: usize,
    pub(crate) removed: usize,
    pub(crate) hunks: Vec<DiffHunk>,
    pub(crate) overflow: bool,
}

#[derive(Clone, Copy)]
enum DiffOp {
    Equal,
    Add,
    Remove,
}

#[derive(Clone, Copy)]
pub(crate) enum ChangeFreshness {
    Bright,
    Dim,
}

pub(crate) fn change_freshness(created_instant: Instant) -> Option<ChangeFreshness> {
    let age = created_instant.elapsed();
    if age <= BRIGHT_CHANGE_WINDOW {
        Some(ChangeFreshness::Bright)
    } else if age <= DIM_CHANGE_WINDOW {
        Some(ChangeFreshness::Dim)
    } else {
        None
    }
}

pub(crate) fn format_clock_hms(time: SystemTime) -> String {
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

pub(crate) fn truncate_label(text: &str, max_chars: usize) -> String {
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

pub(crate) fn hunk_anchor_line(hunk: &DiffHunk, total_lines: usize) -> usize {
    if hunk.end_line > hunk.start_line {
        clamp_line(hunk.start_line, total_lines)
    } else {
        clamp_line(hunk.start_line.saturating_sub(1), total_lines)
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

    let start_line = clamp_line(hunk.start_line, total_lines);
    let end_line = if hunk.end_line > hunk.start_line {
        clamp_line(hunk.end_line.saturating_sub(1), total_lines)
    } else {
        clamp_line(hunk.start_line.saturating_sub(1), total_lines)
    };

    let from = start_line.min(end_line);
    let to = start_line.max(end_line);
    let Some(start_idx) = heading_index_for_line(toc, from) else {
        return Vec::new();
    };
    let end_idx = heading_index_for_line(toc, to).unwrap_or(start_idx);
    (start_idx..=end_idx).collect()
}

pub(crate) fn build_snapshot_diff(
    previous: &RenderedDocument,
    next: &RenderedDocument,
) -> SnapshotDiff {
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

pub(crate) fn compute_line_diff(
    old_lines: &[&str],
    new_lines: &[&str],
    max_cells: usize,
) -> LineDiffResult {
    let prefix = common_prefix_len(old_lines, new_lines);
    let (old_end, new_end) = trim_common_suffix(old_lines, new_lines, prefix);

    let old_mid = &old_lines[prefix..old_end];
    let new_mid = &new_lines[prefix..new_end];

    if old_mid.is_empty() && new_mid.is_empty() {
        return LineDiffResult::default();
    }

    if old_mid.is_empty() {
        return single_hunk_result(prefix, new_mid.len(), 0, false);
    }

    if new_mid.is_empty() {
        return single_hunk_result(prefix, 0, old_mid.len(), false);
    }

    let rows = old_mid.len().saturating_add(1);
    let cols = new_mid.len().saturating_add(1);
    if rows.saturating_mul(cols) > max_cells {
        return single_hunk_result(prefix, new_mid.len(), old_mid.len(), true);
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
