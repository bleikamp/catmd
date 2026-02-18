# PRD: Persistent Watch History Storage for `catmd`

## 1. Summary
This feature persists watch-mode revision history to local state storage so users can reopen `catmd` and continue inspecting recent changes across sessions. History is stored outside the project repo using XDG state directories, with strict retention and size limits to keep disk usage bounded.

## 2. Problem / Opportunity
The current watch timeline is in-memory only. Once `catmd` exits, users lose revision context and cannot inspect what changed earlier in the day unless they manually use Git or external tools. This slows agent-assisted iteration loops and creates context loss between sessions.

## 3. Goals
- Retain recent watch history across app restarts without creating repo commits.
- Keep storage local, bounded, and easy to prune.
- Preserve fast watch-mode UX and timeline navigation.

## 4. Non-goals
- Writing commits or branches in the user repository.
- Remote sync or cloud backup in V1.
- Multi-user history sharing in V1.
- Replacing Git as a long-term source-of-truth history system.

## 5. UX Requirements
### 5.1 Primary Workflows
- Workflow 1: Restore prior timeline on launch
  - User runs `catmd --watch notes.md`.
  - `catmd` restores recent snapshots for that file from local state.
  - Timeline includes restored revisions plus new in-session revisions.
- Workflow 2: Continue live updates with persistent history
  - File changes append new snapshots to memory and on-disk history.
  - User can time-travel through restored and newly-created revisions.
- Workflow 3: Automatic cleanup
  - Old entries are pruned by retention and size caps.
  - User does not manage files manually under normal use.

### 5.2 States / Status
- Restored mode:
  - Status message example: `Restored 34 snapshots (last 5d)`.
- No prior state:
  - Status message example: `No persisted history found`.
- Persistence disabled:
  - Status message example: `Persistent history disabled`.

## 6. Functional Requirements
- FR-1 (Must): The system shall store persistent watch history under XDG state directories.
  - Acceptance criteria: root path resolves to `$XDG_STATE_HOME/catmd` when set, otherwise `~/.local/state/catmd`.
  - Acceptance criteria: watch history path is `<state-root>/watch-history`.

- FR-2 (Must): The system shall persist snapshots for watch-mode file inputs only.
  - Acceptance criteria: no persistence for stdin-only sessions.
  - Acceptance criteria: no writes occur inside the current project repository for history storage.

- FR-3 (Must): The system shall retain data for 5 days by default.
  - Acceptance criteria: default `retention_days = 5`.
  - Acceptance criteria: snapshots older than retention are removed during prune.

- FR-4 (Must): The system shall enforce bounded storage limits.
  - Acceptance criteria: default `global_max_mb = 256`.
  - Acceptance criteria: default `per_file_max_mb = 16`.
  - Acceptance criteria: when caps are exceeded, oldest snapshots are evicted first.

- FR-5 (Must): The system shall restore recent snapshots at watch-mode startup.
  - Acceptance criteria: restored snapshots appear in timeline in chronological order with revision metadata.
  - Acceptance criteria: active view starts at latest snapshot (`LIVE` mode).

- FR-6 (Must): The system shall write history atomically and handle corruption safely.
  - Acceptance criteria: partial writes do not crash startup.
  - Acceptance criteria: corrupt records are skipped with a non-fatal status note.

- FR-7 (Should): The system shall provide a user control to disable persistence.
  - Acceptance criteria: CLI flag exists (`--no-persist-history` or equivalent).
  - Acceptance criteria: disabled mode performs no history reads/writes.

- FR-8 (Should): The system shall provide a user control to clear persisted history.
  - Acceptance criteria: explicit command/flag clears all watch history files under state root.

- FR-9 (Could): The system shall compress stored snapshot payloads.
  - Acceptance criteria: implementation may use zstd or equivalent if needed for cap efficiency.

## 7. Non-functional Requirements
- NFR-1 (Must): Disk usage shall remain bounded by configured caps.
  - Acceptance criteria: long-running usage does not exceed configured global cap after prune completes.

- NFR-2 (Must): Startup restore shall remain interactive for typical datasets.
  - Acceptance criteria: restore + prune completes without visible UI stall for default limits.

- NFR-3 (Should): Persistence overhead shall not meaningfully slow watch reloads.
  - Acceptance criteria: write path is lightweight and does not block rendering loop for normal docs.

## 8. Storage Model
- State root:
  - `$XDG_STATE_HOME/catmd` or fallback `~/.local/state/catmd`
- History directory:
  - `<state-root>/watch-history`
- File identity:
  - Canonical file path mapped to a stable key (hash or escaped path).
- Stored record fields (minimum):
  - timestamp
  - revision id
  - markdown/source payload
  - diff summary (`+/-`, hunks count, top section)
- Index metadata:
  - per-file byte usage
  - global byte usage
  - last-pruned timestamp

## 9. Versions and Scope
### V1 (2026-02-18)
- Decisions made:
  - Use XDG state directories for storage.
  - Retention defaults to 5 days.
  - Size caps default to 256MB global and 16MB per file.
  - No repo-level Git integration.
- Scope:
  - Persist/restore watch snapshots.
  - Startup + periodic prune by age and size.
  - Opt-out control for persistence.

### V2 (2026-03-xx)
- Scope:
  - Improved compression and compaction strategy.
  - Clear-history command polish and finer-grained controls.
  - Better file identity handling across renames.

### V3 (2026-04-xx)
- Scope:
  - Optional export/import bundle for sharing sessions.
  - Optional integration hooks with external tools.

## 10. Risks and Mitigations
- Risk: State files grow too quickly for heavy write streams.
  - Mitigation: strict caps + prune-on-write + optional compression.

- Risk: Path identity breaks on rename/move.
  - Mitigation: support canonical path plus optional content fingerprint heuristics in V2.

- Risk: Users confuse app state with repo data.
  - Mitigation: document storage path clearly and never write under repository root.

## 11. Open Questions
- Should persistence be enabled by default or opt-in in first release?
- Which CLI surface is preferred for cleanup: `--clear-history` or dedicated subcommand?
- Should restore load exactly up to limits or stop at a smaller startup budget?

## 12. Decision Log
- 2026-02-18: Persistent history will be local state only, not repo Git commits.
- 2026-02-18: Use XDG state path with fallback to `~/.local/state/catmd`.
- 2026-02-18: Default retention/caps set to `5 days`, `256MB global`, `16MB per-file`.
