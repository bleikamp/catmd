# catmd

A fast terminal Markdown viewer for agent-heavy workflows.

`catmd` is designed for the "just generated some Markdown, let me inspect it now" loop:
- open a file in an interactive pager
- pipe markdown from `stdin` for one-shot rendering
- navigate with vim keys
- inspect a toggleable TOC
- open links (including local markdown files)
- auto-reload with `--watch`

## Status

Early v1. Internal/dev-first.

## Goals

- Strong GFM support (tables, task lists, fenced code blocks, strikethrough, autolinks)
- Excellent terminal ergonomics for reading long docs
- Minimal cognitive overhead and predictable defaults

## Non-goals (v1)

- Full GUI/TUI app complexity
- Perfect pixel-equivalence with browser HTML rendering
- Terminal image rendering protocols (kitty/iterm image APIs)

## Install (dev)

```bash
cargo install --path .
```

Or run directly:

```bash
cargo run -- README.md
```

## Usage

```bash
# Interactive pager for files (when output is a TTY)
catmd notes.md

# Read from stdin (plain render by default)
cat README.md | catmd

# Force interactive mode
cat README.md | catmd --interactive

# Force plain output
catmd notes.md --plain

# Auto-reload when file changes
catmd notes.md --watch

# Keep up to 200 in-memory watch snapshots
catmd notes.md --watch --history 200
```

### Input behavior

- File input + TTY output -> interactive pager by default
- `stdin` input -> plain output by default
- `--interactive` forces pager mode
- `--plain` forces non-interactive output

## Keybindings (interactive)

- `j` / `k`: scroll down/up
- `Ctrl-d` / `Ctrl-u`: half-page down/up
- `g` / `G`: top/bottom
- `/`: search (incremental as you type)
- `n` / `N`: next/previous search match
- `?`: toggle keyboard shortcuts help
- `A` or `Ctrl-a`: quick-add a new `@agent` task (appends to current file)
- `t`: toggle TOC sidebar
- `a`: toggle Agent Inbox sidebar
- `[` / `]`: jump to previous/next heading
- `j` / `k` (when TOC is open): move TOC selection
- `j` / `k` (when Agent Inbox is open): move task selection
- `Enter` (when TOC is open): jump to selected TOC heading
- `Enter` (when Agent Inbox is open): jump to selected unresolved agent task
- `Tab` / `Shift-Tab`: next/previous link
- `Enter`: open selected link (when TOC is closed)
- `o`: open selected link externally (browser/system opener)
- `Backspace`: go back in local markdown backstack
- `{` / `}`: previous/next unresolved `@agent` task
- `v`: toggle timeline dock (watch mode)
- `h` / `l` or `Left` / `Right`: older/newer revision (watch mode)
- `L`: jump back to live/latest revision (watch mode)
- `(` / `)`: previous/next changed hunk (watch mode)
- `q`: quit

## Link behavior

- Relative `.md` links open inside `catmd` and push the current document onto a backstack
- `http` / `https` links open in the system browser
- Other local paths open via the system opener

## Images

Images render as placeholders with alt text and path:

```text
[image: architecture diagram] (./assets/arch.png)
```

## Watch mode

`--watch` reloads file-backed documents when the source file changes.

- scroll position is preserved when possible
- TOC and links refresh after reload
- watch mode is only available for file input
- in-memory revision history is kept with `--history <N>` (default `50`)
- timeline dock shows revision id, timestamp, `+/-` summary, touched section count, and top changed section
- status bar shows `LIVE` vs `HISTORY` mode

## Agent Collaboration

Agent tasks are recognized from checklist lines tagged with `@agent`:

```md
- [ ] @agent clean up this section
- [x] @agent done
```

- unresolved tasks are highlighted inline
- completed tasks are shown in a subdued style
- Agent Inbox shows unresolved items and jumps to them by line
- status bar shows `agent: open/total` counts when tasks exist
- quick capture: press `A` (or `Ctrl-a`), type task text, press `Enter` to append `- [ ] @agent ...`

## Roadmap

- Anchor link jumps (`#section-name`)
- Better table rendering and wrapping
- Theme presets
- Homebrew tap / release artifacts

## License

MIT
