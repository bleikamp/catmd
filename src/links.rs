use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, Result};

fn resolve_path(path: &Path, current_doc: Option<&Path>) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }

    if let Some(parent) = current_doc.and_then(Path::parent) {
        return parent.join(path);
    }

    path.to_path_buf()
}

fn is_markdown_path(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .map(str::to_ascii_lowercase);
    matches!(ext.as_deref(), Some("md" | "markdown" | "mdx"))
}

pub(crate) fn system_open<S: AsRef<OsStr>>(arg: S) -> Result<()> {
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

#[derive(Clone, Debug)]
pub(crate) enum LinkAction {
    InternalMarkdown(PathBuf),
    ExternalUrl(String),
    ExternalPath(PathBuf),
    Anchor(String),
    Unknown(String),
}

pub(crate) fn classify_link(target: &str, current_doc: Option<&Path>) -> LinkAction {
    if target.starts_with("http://") || target.starts_with("https://") {
        return LinkAction::ExternalUrl(target.to_string());
    }

    if target.starts_with('#') {
        return LinkAction::Anchor(target.to_string());
    }

    let path_part = target.split_once('#').map_or(target, |(path, _)| path);

    if path_part.is_empty() {
        return LinkAction::Anchor(target.to_string());
    }

    let resolved = resolve_path(Path::new(path_part), current_doc);
    if is_markdown_path(&resolved) {
        return LinkAction::InternalMarkdown(resolved);
    }

    if resolved.exists() {
        return LinkAction::ExternalPath(resolved);
    }

    LinkAction::Unknown(target.to_string())
}
