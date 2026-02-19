use std::fs;
use std::io::{self, IsTerminal, Read};
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use clap::Parser;

const HISTORY_PARSE_ERR: &str = "--history must be a positive integer";
const HISTORY_MIN_ERR: &str = "--history must be at least 1";
const NO_INPUT_ERR: &str = "No input provided. Pass a markdown file or pipe markdown into stdin.";
const READ_STDIN_ERR: &str = "Failed to read markdown from stdin";

fn parse_history(value: &str) -> std::result::Result<usize, String> {
    let parsed: usize = value.parse().map_err(|_| HISTORY_PARSE_ERR.to_string())?;
    if parsed == 0 {
        return Err(HISTORY_MIN_ERR.to_string());
    }
    Ok(parsed)
}

#[derive(Debug, Parser)]
#[command(
    name = "catmd",
    version,
    about = "Render markdown for terminal workflows"
)]
pub(crate) struct Cli {
    /// Markdown file path. Use '-' to read from stdin.
    pub(crate) input: Option<String>,

    /// Force interactive pager mode.
    #[arg(short, long)]
    pub(crate) interactive: bool,

    /// Force plain stdout rendering.
    #[arg(long)]
    pub(crate) plain: bool,

    /// Reload when the file changes (file input only).
    #[arg(long)]
    pub(crate) watch: bool,

    /// Number of in-memory snapshots to keep while watching.
    #[arg(long, default_value_t = 50, value_parser = parse_history)]
    pub(crate) history: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct LoadResult {
    pub(crate) path: Option<PathBuf>,
    pub(crate) source: String,
}

#[derive(Clone, Debug)]
pub(crate) enum InputSource {
    File(PathBuf),
    Stdin,
}

pub(crate) fn detect_input(cli: &Cli) -> Result<InputSource> {
    match cli.input.as_deref() {
        Some("-") => Ok(InputSource::Stdin),
        Some(path) => Ok(InputSource::File(PathBuf::from(path))),
        None if io::stdin().is_terminal() => Err(anyhow!(NO_INPUT_ERR)),
        None => Ok(InputSource::Stdin),
    }
}

pub(crate) fn read_input(source: &InputSource) -> Result<LoadResult> {
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
                .context(READ_STDIN_ERR)?;
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

pub(crate) fn default_interactive(input: &InputSource) -> bool {
    matches!(input, InputSource::File(_)) && is_tty_stdout()
}
