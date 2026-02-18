# PRD: Watch History + Diff Time Travel for `catmd`

## 1. Summary
This feature adds a change-aware watch experience to `catmd` so users can immediately see what changed, where it changed, and navigate revision history over time. In `--watch` mode, the app will capture in-memory snapshots, compute diffs between snapshots, and expose a timeline UI for moving backward/forward through revisions. The intended outcome is faster collaborative editing loops when Markdown is being regenerated frequently.

## 2. Problem / Opportunity
Current `--watch` reloads are useful but not enough when multiple edits happen quickly. Users must re-scan the whole document to determine what changed and where. This creates friction for agent-assisted writing workflows where iterative updates are frequent and users need rapid confidence in recent changes.

## 3. Goals
- Make recent changes obvious within 1-2 seconds after each reload.
- Allow keyboard-only navigation backward/forward through recent revisions.
- Preserve current `catmd` simplicity and responsiveness.

## 4. Non-goals
- Git-aware history (commits/branches) in V1.
- Multi-user network collaboration in V1.
- Persistent history across app restarts in V1.

## 5. UX Requirements
### 5.1 Primary Workflows
- Workflow 1: Live watch + change awareness
  - User opens `catmd --watch FILE.md`.
  - File changes trigger reload.
  - Changed sections/lines are clearly indicated using TOC freshness markers + transient in-content highlights.
- Workflow 2: Time travel through revisions
  - User opens timeline panel and moves to older/newer snapshots.
  - User inspects specific revision content and returns to live/latest.
- Workflow 3: Diff-hunk navigation
  - User jumps to next/previous changed hunk within selected revision.

### 5.2 Screens / States / Routes
- Base layout:
  - Left sidebar slot: TOC (optional/toggleable).
  - Main pane: rendered Markdown.
  - Bottom region: timeline dock (when open) + status bar.
- Timeline-open layout:
  - Timeline appears as a bottom dock above the status bar.
  - Timeline uses full content width and does not replace the TOC.
  - Timeline rows are newest-first with richer revision metadata enabled by wider layout.
- Live mode (latest snapshot):
  - Status bar prefix: `LIVE r{latest}`.
  - File changes auto-advance the view to latest snapshot.
  - TOC shows freshness markers for sections touched by latest revision.
- Historical mode (older snapshot selected):
  - Status bar prefix: `HISTORY r{selected} ({n} behind LIVE)`.
  - Auto-follow is paused until user jumps back to live/latest.
  - Main pane renders selected historical snapshot content.
- Empty timeline state:
  - Shown when only one snapshot exists.
  - Dock text: "No prior revisions yet".

### 5.3 Accessibility / Platform
- Terminal-first, keyboard-only interaction.
- Existing key model (vim + arrows) shall remain usable.
- No mouse required for any timeline action.

### 5.4 Performance (optional)
- Reload processing (parse + diff + state update) should remain interactive for typical docs (up to ~5,000 lines).
- Snapshot navigation should feel instant (<50ms target for local state switch).

### 5.5 Change Indication Specification
- Section-level indication (TOC):
  - Recently changed section marker: `●` prefix before heading title.
  - Marker freshness buckets:
    - `0-2s` after reload: bright/high-contrast marker.
    - `2-15s` after reload: dim marker.
    - `>15s`: marker cleared.
  - Optional summary suffix in timeline mode: `(+A/-R)` per touched section, where available.
- Line-level indication (content pane):
  - Changed lines receive immediate high-contrast background highlight for 2 seconds.
  - Highlight decays to low-contrast background tint for the next 13 seconds.
  - After 15 seconds total, line returns to normal style.
  - When viewing historical snapshots, highlight state is derived from that selected revision (not current live revision).
- Hunk-level indication:
  - First visible line of each hunk gets a left-gutter marker (for example `▌`).
  - `next/prev hunk` navigation lands at the first line with this marker.
- Terminal compatibility fallback:
  - If color is unavailable, use symbol-only indicators (`●`, `▌`, and status labels) so changes remain distinguishable.

### 5.6 Time Travel UI Specification
- Timeline panel row format (example):
  - `r042  10:31:08  +12/-3  h:2  top:## UX Requirements`
  - `r041  10:30:52  +2/-0   h:1  top:## Goals`
- Timeline row fields:
  - Revision id/index (monotonic in session).
  - Local timestamp (HH:MM:SS).
  - Change summary (`+added/-removed` lines).
  - Number of touched heading ranges (`h:{count}`).
  - Top changed section label (best effort truncation in narrow terminals).
- Dock sizing:
  - Default dock height: 6 rows.
  - Minimum dock height: 3 rows.
  - Optional future control: increase/decrease dock height with keyboard.
- Selection and mode behavior:
  - Selected row is highlighted with inverse/background style.
  - Pressing open/confirm on a row switches to `HISTORY` mode for that revision.
  - Jump-to-live action returns to newest row and `LIVE` mode.
- Status bar in timeline/history modes:
  - Live example: `LIVE r042 | +12/-3 | sections:2 | watch:on`
  - History example: `HISTORY r039 (3 behind) | +4/-1 | hunks:2`

## 6. Functional Requirements
- FR-1 (Must): The system shall maintain an in-memory ring buffer of document snapshots while in `--watch` mode.
  - Acceptance criteria: configurable `--history <N>` (default 50); oldest snapshots are evicted when capacity is exceeded.

- FR-2 (Must): The system shall compute a line-level diff for each new snapshot against the previous snapshot.
  - Acceptance criteria: each snapshot stores added/removed/changed hunk metadata and line ranges.

- FR-3 (Must): The system shall map diff hunks to heading ranges (`h1-h3`) for section-level change visibility.
  - Acceptance criteria: TOC entries touched by latest diff are marked with freshness states (`0-2s`, `2-15s`, cleared after `>15s`).

- FR-4 (Must): The system shall provide a timeline panel in watch mode showing revision order and metadata.
  - Acceptance criteria: each row includes revision index, local timestamp, change summary (`+/-` line counts), touched heading count, and top changed section label when available.

- FR-5 (Must): The system shall allow revision navigation backward and forward using keyboard shortcuts.
  - Acceptance criteria: user can move to previous/newer snapshots and return to latest live snapshot.

- FR-6 (Must): The system shall support hunk-level navigation for the selected revision.
  - Acceptance criteria: next/previous hunk commands move viewport to corresponding changed range.

- FR-7 (Must): The system shall clearly indicate whether the user is viewing live/latest content or a historical snapshot.
  - Acceptance criteria: status bar includes mode label (`LIVE` or `HISTORY rN`) and revision distance from latest when in `HISTORY`.

- FR-8 (Should): The system shall apply transient visual emphasis to newly changed lines after reload.
  - Acceptance criteria: newly changed lines use high-contrast highlight for 2 seconds and low-contrast tint through 15 seconds total.

- FR-9 (Should): The system shall preserve scroll context intelligently when switching revisions.
  - Acceptance criteria: when possible, viewport stays near equivalent logical area; otherwise jumps to first changed hunk.

- FR-10 (Could): The system shall expose diff granularity toggle (line-only vs line+word) for future fidelity.
  - Acceptance criteria: flag exists behind experimental gate or deferred implementation marker.

## 7. Non-functional Requirements (optional)
- NFR-1 (Must): The system shall cap memory usage via bounded snapshot history.
  - Acceptance criteria: no unbounded growth during long watch sessions.

- NFR-2 (Must): The system shall degrade gracefully on very large diffs.
  - Acceptance criteria: if diff cost exceeds threshold, app remains usable and shows a fallback status message.

## 8. Versions and Scope
### V1 (2026-02-18)
- Decisions made:
  - Feature is limited to `--watch` mode.
  - Snapshot storage is in-memory only.
  - Diff baseline is line-level.
- Scope:
  - Ring buffer snapshots (`--history <N>` default 50).
  - Timeline panel with keyboard navigation.
  - Revision mode indicator in status bar.
  - TOC recent-change markers from heading-range mapping.
  - Hunk navigation commands.
- Notes:
  - Keep keybindings minimal and consistent with existing controls.

### V2 (2026-03-xx)
- Decisions made:
  - Consider optional persistence to disk.
- Scope:
  - Save/restore watch session history.
  - Better mapping when headings are renamed/reordered.
  - Optional word-level inline diff rendering.
- Notes:
  - Validate performance impact before enabling by default.

### V3 (2026-04-xx)
- Decisions made:
  - Collaboration features only after V1/V2 UX stabilizes.
- Scope:
  - Multi-source feeds (agent output stream + file watch).
  - Session export/share format.
- Notes:
  - Out of scope until core local workflow is stable.

## 9. Post-V3 / Future Ideas
- Side-by-side diff view for selected snapshots.
- Section-only history filter.
- "Pin this revision" bookmarks.

## 10. Dependencies (optional)
- Internal:
  - Existing markdown parse/render pipeline and TOC extraction.
  - Existing watch pipeline (`notify`) and TUI state model.
- External:
  - Rust diff crate (TBD, likely lightweight line diff).

## 11. Risks and Mitigations
- Risk: Keybinding conflicts with current navigation.
  - Mitigation: define timeline-specific mode keys; document clearly in help/status.

- Risk: Frequent writes produce noisy snapshots.
  - Mitigation: debounce/coalesce rapid events before snapshot creation.

- Risk: Large docs degrade responsiveness.
  - Mitigation: bounded history, incremental metadata updates, fallback to summary-only diff display.

## 12. Open Questions
- What exact keybindings should V1 use for:
  - timeline panel toggle
  - previous/next revision
  - return to live/latest
  - previous/next hunk
- Should diff metadata include word-level details in V1 or stay line-only?
- Should `--history` remain watch-only or be accepted in non-watch mode as no-op/error?
- Should timeline rows include touched heading titles or only counts in V1?
- Should timeline dock height be fixed (default 6) or user-adjustable in V1?

## 13. Decision Log
- 2026-02-18: Watch mode needs stronger change visibility and revision navigation support.
- 2026-02-18: V1 will prioritize in-memory snapshots and keyboard-first timeline navigation.
- 2026-02-18: Persistence and advanced diff fidelity are deferred beyond V1.
