//! The text every managed agent starts with: the shipped orchestration
//! skill plus a role banner with the facts of this run. Adapters put it into
//! the system prompt (notagent, Claude) or in front of the task document
//! (Codex).

use super::harness::AgentRole;

/// The skill as shipped, frontmatter stripped.
pub const SKILL_SOURCE: &str = include_str!("../../resources/skills/notmux-orchestration/SKILL.md");

pub fn skill_body() -> &'static str {
    let s = SKILL_SOURCE;
    if let Some(rest) = s.strip_prefix("---\n")
        && let Some(end) = rest.find("\n---\n")
    {
        return rest[end + 5..].trim_start();
    }
    s
}

/// Facts the banner names.
#[derive(Clone, Debug, Default)]
pub struct RunFacts<'a> {
    pub run_id: &'a str,
    pub root_id: Option<&'a str>,
    pub name: &'a str,
    pub cwd: &'a str,
    pub harness: &'a str,
    pub total_start_budget: Option<u32>,
    /// The first assignment's task ID, for workers.
    pub task_id: Option<&'a str>,
    pub task_file: Option<&'a str>,
}

pub fn system_prompt(role: AgentRole, facts: &RunFacts<'_>) -> String {
    let mut out = String::new();
    match role {
        AgentRole::Root => {
            out.push_str("# You are the NotMux orchestrator (root)\n\n");
            out.push_str(&format!("Run ID: {}\n", facts.run_id));
            out.push_str(&format!("Harness: {}\n", facts.harness));
            out.push_str(&format!("Working directory: {}\n", facts.cwd));
            if let Some(budget) = facts.total_start_budget {
                out.push_str(&format!("Worker start budget: {budget}\n"));
            }
            out.push_str(
                "\nYou direct up to four workers through `notmux agent`; you never do their work \
                 in their panes and they never spawn workers. The full protocol follows.\n\n",
            );
        }
        AgentRole::Worker => {
            out.push_str("# You are a NotMux worker\n\n");
            out.push_str(&format!("Run ID: {}\n", facts.run_id));
            if let Some(root) = facts.root_id {
                out.push_str(&format!("Root (orchestrator) run ID: {root}\n"));
            }
            out.push_str(&format!("Name: {}\n", facts.name));
            out.push_str(&format!("Harness: {}\n", facts.harness));
            out.push_str(&format!("Working directory: {}\n", facts.cwd));
            if let Some(task_id) = facts.task_id {
                out.push_str(&format!("Current task ID: {task_id}\n"));
            }
            if let Some(task_file) = facts.task_file {
                out.push_str(&format!("Assignment file: {task_file}\n"));
            }
            out.push_str(
                "\nYou work on one assignment at a time, report it with `notmux agent finish`, \
                 then wait with `notmux agent next`. You never start other agents. The full \
                 protocol follows.\n\n",
            );
        }
    }
    out.push_str(skill_body());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_body_has_no_frontmatter_and_names_the_commands() {
        let body = skill_body();
        assert!(body.starts_with("# NotMux agent orchestration"));
        for verb in ["spawn", "assign", "next", "finish", "wait", "inbox", "message", "stop"] {
            assert!(body.contains(&format!("notmux agent {verb}")), "{verb}");
        }
    }

    #[test]
    fn prompts_carry_the_run_facts() {
        let facts = RunFacts {
            run_id: "r",
            root_id: Some("root"),
            name: "Worker A",
            cwd: "/w",
            harness: "claude",
            total_start_budget: None,
            task_id: Some("t"),
            task_file: Some("/w/task.md"),
        };
        let worker = system_prompt(AgentRole::Worker, &facts);
        assert!(worker.contains("Root (orchestrator) run ID: root"));
        assert!(worker.contains("Current task ID: t"));
        assert!(worker.contains("## If you are a worker"));
        let root = system_prompt(
            AgentRole::Root,
            &RunFacts {
                total_start_budget: Some(6),
                ..facts
            },
        );
        assert!(root.contains("Worker start budget: 6"));
        assert!(root.starts_with("# You are the NotMux orchestrator"));
    }
}
