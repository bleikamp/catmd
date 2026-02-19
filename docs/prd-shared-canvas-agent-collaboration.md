# PRD: Shared Canvas Collaboration for `catmd`

## 1. Summary
This feature evolves `catmd` from a read-only Markdown viewer into a lightweight shared canvas between a human and a coding agent. The user keeps `catmd` open in split view while iterating with an agent, leaving embedded notes/tasks for the agent and tracking completion in the same document. The intended outcome is faster, less lossy human-agent loops without introducing heavyweight multi-user sync.

## 2. Problem / Opportunity
In agent-assisted workflows, the Markdown file is often the working surface for requirements, TODOs, and cleanup notes. Today, notes to the agent are easy to miss because they are mixed with regular content and there is no first-class way to track unresolved vs resolved agent requests. Users also lack quick navigation across these collaboration markers, which creates context switching overhead.

## 3. Goals
- Make agent-targeted notes explicit and easy to scan.
- Provide a keyboard-first "inbox" for unresolved agent tasks.
- Support a simple lifecycle (`open`, `done`) so users can confirm cleanup happened.
- Fit naturally into existing `--watch` split-view workflows.

## 4. Non-goals
- Real-time networked multi-user collaboration in V1.
- Full inline editing of arbitrary Markdown in `catmd` in V1.
- LLM orchestration/execution inside `catmd` in V1.
- Replacing external coding-agent tools or chat UX.

## 5. Success Metrics (optional)
- 80%+ of agent notes in pilot docs use the supported marker format after 2 weeks.
- Median time to navigate to next unresolved agent task is under 1 second.
- 50% reduction in "forgotten cleanup note" incidents in user-reported sessions.

## 6. Users / Personas
- Primary: developers using a coding agent with `catmd` open in a side pane.
- Secondary: technical writers or PMs collaborating with agents on long Markdown docs.

## 7. UX Requirements
### 7.1 Primary Workflows
- Workflow 1: Leave an agent task in the markdown file
  - User writes an agent-addressable task in Markdown using the supported syntax.
  - `catmd` highlights it as an unresolved collaboration item.
- Workflow 2: Triage unresolved tasks
  - User opens an "Agent Inbox" panel and sees unresolved items sorted by document order.
  - User jumps to each task directly from the panel.
- Workflow 3: Confirm completion
  - After the agent updates the file, tasks marked complete are shown as resolved.
  - Inbox count drops and status bar reflects remaining unresolved items.

### 7.2 Screens / States / Routes
- Main content view:
  - Collaboration items are visually distinct from regular task lists.
  - Each item shows status badge (`OPEN`, `DONE`) and optional metadata (owner/tag).
- Agent Inbox panel (toggleable):
  - Displays unresolved items with line anchor and truncated text.
  - Selection jumps viewport to the associated line.
- Status bar:
  - Shows summary, e.g., `agent: 3 open / 7 total`.
- Empty states:
  - `No agent tasks found` when no markers exist.
  - `All agent tasks complete` when zero unresolved tasks remain.

### 7.3 Accessibility / Platform
- Terminal-first and keyboard-only.
- All collaboration actions reachable without mouse input.
- Maintain parity with existing vim-style navigation conventions.

### 7.4 Performance (optional)
- Agent task extraction should add negligible overhead to watch reloads for typical files (<5,000 lines).
- Opening or navigating the inbox should feel instant (<50ms local state switch target).

## 8. Functional Requirements
- FR-1 (Must): The system shall recognize agent-task markers in Markdown using a documented V1 syntax.
  - Acceptance criteria: V1 supports checklist syntax `- [ ] @agent ...` (open) and `- [x] @agent ...` (done).
  - Acceptance criteria: parser ignores case for `@agent` tag matching.

- FR-2 (Must): The system shall extract agent-task metadata for each marker.
  - Acceptance criteria: extracted fields include status (`open`/`done`), source line number, and text body.
  - Acceptance criteria: extraction updates on every reload in `--watch` mode.

- FR-3 (Must): The system shall render unresolved agent tasks with distinct visual treatment in content view.
  - Acceptance criteria: unresolved tasks are visually differentiable from non-agent checklist items.
  - Acceptance criteria: resolved agent tasks are shown with a subdued style.

- FR-4 (Must): The system shall provide an Agent Inbox panel listing unresolved tasks.
  - Acceptance criteria: panel is keyboard-toggleable and shows unresolved count.
  - Acceptance criteria: selecting an inbox row jumps to the corresponding line in the document.

- FR-5 (Must): The system shall provide next/previous unresolved agent-task navigation commands.
  - Acceptance criteria: users can jump between unresolved agent tasks without opening TOC.
  - Acceptance criteria: status bar confirms current task index, e.g., `agent task 2/5`.

- FR-6 (Should): The system shall show collaboration summary in the status bar.
  - Acceptance criteria: status bar includes `open/total` agent task counts when markers exist.

- FR-7 (Should): The system shall support configurable marker tag aliases.
  - Acceptance criteria: users may configure aliases such as `@assistant` in addition to `@agent`.
  - Acceptance criteria: default behavior requires no config.

- FR-8 (Could): The system shall support comment-style agent markers that do not render in final Markdown.
  - Acceptance criteria: experimental support for HTML comment markers (format TBD) behind a feature flag.

- FR-9 (Won't, V1): The system shall not directly trigger coding-agent actions from inside `catmd`.
  - Acceptance criteria: no built-in "run agent" or LLM API action in V1.

## 9. Non-functional Requirements (optional)
- NFR-1 (Must): Existing keybindings and watch timeline behavior shall remain intact.
  - Acceptance criteria: regression tests/manual checks confirm current navigation still works.

- NFR-2 (Must): Agent-task parsing shall degrade gracefully on malformed markers.
  - Acceptance criteria: malformed lines are ignored without crashing and without blocking render.

- NFR-3 (Should): Collaboration markers shall remain readable in low-color terminals.
  - Acceptance criteria: symbol/text fallbacks exist when color contrast is limited.

## 10. Versions and Scope
### V1 (2026-02-19)
- Decisions made:
  - Start with explicit checklist-tag syntax (`- [ ] @agent ...`).
  - Focus on visibility + navigation, not editing or automation.
  - Keep feature local and file-based; no network sync.
- Scope:
  - Marker extraction and metadata.
  - Inline visual treatment for `open` vs `done`.
  - Agent Inbox panel + next/previous unresolved navigation.
  - Status bar summary (`open/total`).
- Notes:
  - Designed to ship quickly and validate behavior with existing split-view workflows.

### V2 (2026-03-xx)
- Decisions made:
  - Expand marker flexibility after V1 adoption signal.
- Scope:
  - Alias configuration (`@assistant`, team-specific tags).
  - Optional comment-only marker syntax for non-rendered collaboration notes.
  - Filters in inbox (open-only, done-only, by heading section).
- Notes:
  - Preserve backward compatibility with V1 marker syntax.

### V3 (2026-04-xx)
- Decisions made:
  - Consider deeper agent workflow only after V1/V2 prove useful.
- Scope:
  - Optional local action hooks (copy task to clipboard, emit structured event).
  - Cross-file canvas view aggregating agent tasks from multiple docs.
  - Optional lightweight thread model (`request` / `response` pairs).
- Notes:
  - Still no required cloud service in base offering.

## 11. Post-V3 / Future Ideas
- Per-task timestamps (`created`, `resolved`) inferred from timeline revisions.
- "Needs human review" reciprocal tag (`@human`) for agent-to-user handoff loops.
- Export unresolved tasks as markdown checklist summary.

## 12. Dependencies (optional)
- Internal:
  - Existing markdown parsing and rendered line model.
  - Existing watch reload pipeline and status bar.
  - Existing timeline/diff navigation patterns for keyboard UX consistency.
- External:
  - None required for V1 beyond current crate set.

## 13. Risks and Mitigations
- Risk: Marker syntax feels too rigid and users keep writing free-form comments.
  - Mitigation: start with one obvious format and add aliases in V2.

- Risk: Feature duplicates existing checklist behavior and adds little value.
  - Mitigation: prioritize inbox + jump navigation, not just styling differences.

- Risk: Keybinding collisions with current navigation.
  - Mitigation: reserve minimal new keys and document in status/help text.

## 14. Open Questions
- Which keybindings should control:
  - Agent Inbox toggle
  - Next/previous unresolved task
  - Jump from inbox item to content (reuse `Enter`?)
- Should agent tasks be restricted to checklist items only in V1, or include headings/paragraph annotations?
- Should status bar counts include only visible file scope or all loaded snapshots in history mode?
- Should `@agent` matching be exact tag token or substring match?

## 15. Decision Log
- 2026-02-19: Prioritize lightweight shared-canvas semantics over deep automation.
- 2026-02-19: V1 collaboration primitive is `@agent` checklist items with open/done state.
- 2026-02-19: Inbox + keyboard navigation are required in V1 to reduce missed cleanup tasks.
