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
- `t`: toggle TOC sidebar
- `[` / `]`: jump to previous/next heading
- `j` / `k` (when TOC is open): move TOC selection
- `Enter` (when TOC is open): jump to selected TOC heading
- `Tab` / `Shift-Tab`: next/previous link
- `Enter`: open selected link (when TOC is closed)
- `o`: open selected link externally (browser/system opener)
- `Backspace`: go back in local markdown backstack
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

## Roadmap

- Anchor link jumps (`#section-name`)
- Better table rendering and wrapping
- Theme presets
- Homebrew tap / release artifacts

## License

MIT
