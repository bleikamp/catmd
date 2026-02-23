use crate::markdown::RenderedDocument;

const AGENT_TAG: &str = "@agent";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AgentTaskState {
    Open,
    Done,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AgentTask {
    pub(crate) line: usize,
    pub(crate) text: String,
    pub(crate) state: AgentTaskState,
}

impl AgentTask {
    pub(crate) fn is_open(&self) -> bool {
        matches!(self.state, AgentTaskState::Open)
    }
}

fn is_tag_boundary(ch: Option<char>) -> bool {
    match ch {
        None => true,
        Some(value) => !(value.is_ascii_alphanumeric() || value == '_'),
    }
}

fn contains_agent_tag(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    for (index, _) in lower.match_indices(AGENT_TAG) {
        let before = lower[..index].chars().next_back();
        let after = lower[index + AGENT_TAG.len()..].chars().next();
        if is_tag_boundary(before) && is_tag_boundary(after) {
            return true;
        }
    }
    false
}

fn parse_agent_task_line(line: &str) -> Option<(AgentTaskState, &str)> {
    let trimmed = line.trim_start();
    if let Some(body) = trimmed.strip_prefix("- [ ] ") {
        return Some((AgentTaskState::Open, body));
    }
    if let Some(body) = trimmed
        .strip_prefix("- [x] ")
        .or_else(|| trimmed.strip_prefix("- [X] "))
    {
        return Some((AgentTaskState::Done, body));
    }
    None
}

pub(crate) fn extract_agent_tasks(rendered: &RenderedDocument) -> Vec<AgentTask> {
    rendered
        .lines
        .iter()
        .enumerate()
        .filter_map(|(line, rendered_line)| {
            let (state, body) = parse_agent_task_line(&rendered_line.plain)?;
            if !contains_agent_tag(body) {
                return None;
            }
            Some(AgentTask {
                line,
                text: body.trim().to_string(),
                state,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown::{RenderedLine, StyledSegment};

    fn doc(lines: &[&str]) -> RenderedDocument {
        RenderedDocument {
            lines: lines
                .iter()
                .map(|line| RenderedLine {
                    segments: vec![StyledSegment {
                        text: (*line).to_string(),
                        style: Default::default(),
                    }],
                    plain: (*line).to_string(),
                })
                .collect(),
            toc: Vec::new(),
            links: Vec::new(),
        }
    }

    #[test]
    fn extract_agent_tasks_detects_open_and_done() {
        let rendered = doc(&[
            "- [ ] @agent polish this section",
            "- [x] @Agent cleanup complete",
            "- [ ] @AGENT: verify links",
        ]);

        let tasks = extract_agent_tasks(&rendered);
        assert_eq!(tasks.len(), 3);
        assert_eq!(tasks[0].state, AgentTaskState::Open);
        assert_eq!(tasks[1].state, AgentTaskState::Done);
        assert_eq!(tasks[2].state, AgentTaskState::Open);
    }

    #[test]
    fn extract_agent_tasks_requires_tag_boundary() {
        let rendered = doc(&[
            "- [ ] @agentic should be ignored",
            "- [ ] prep @agent should be included",
        ]);

        let tasks = extract_agent_tasks(&rendered);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].line, 1);
        assert_eq!(tasks[0].text, "prep @agent should be included");
    }

    #[test]
    fn extract_agent_tasks_ignores_non_checklist_lines() {
        let rendered = doc(&[
            "@agent plain paragraph",
            "- @agent list but not task",
            "- [ ] plain task without tag",
        ]);

        let tasks = extract_agent_tasks(&rendered);
        assert!(tasks.is_empty());
    }
}
