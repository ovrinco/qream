use crate::{
    Result,
    agent::{AgentInfo, AgentKind, AgentState, BlockedKind, Evidence},
    events::now,
};
use regex::Regex;
use serde::Deserialize;
#[derive(Deserialize)]
struct File {
    /// Helper processes the harness keeps for the whole session (Codex 0.162's `node_repl`): never a job.
    #[serde(default)]
    resident: Vec<String>,
    rule: Vec<Rule>,
}
#[derive(Deserialize)]
struct Rule {
    name: String,
    state: AgentState,
    blocked_kind: Option<BlockedKind>,
    pattern: String,
    #[serde(default)]
    interactive_ready: bool,
    #[serde(default)]
    multiline: bool,
    #[serde(default)]
    verified: bool,
    #[serde(default)]
    requires: Vec<String>,
}
struct Compiled {
    rule: Rule,
    pattern: Regex,
    requires: Vec<Regex>,
}
pub struct Detector {
    rules: Vec<Compiled>,
    resident: Vec<String>,
}
impl Detector {
    pub fn from_toml(data: &str) -> Result<Self> {
        let file: File = toml::from_str(data)?;
        let File { resident, rule } = file;
        let rules = rule
            .into_iter()
            .map(|rule| {
                Ok(Compiled {
                    pattern: Regex::new(&rule.pattern)?,
                    requires: rule
                        .requires
                        .iter()
                        .map(|text| Regex::new(text))
                        .collect::<std::result::Result<Vec<_>, _>>()?,
                    rule,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self { rules, resident })
    }
    /// Whether a process is one of the harness's resident helpers (or runs under one).
    pub fn is_resident(&self, pid: u32) -> bool {
        resident_process(pid, &self.resident, &proc_entry)
    }
    pub fn verified(&self, rule_name: &str) -> Option<bool> {
        self.rules
            .iter()
            .find(|compiled| compiled.rule.name == rule_name)
            .map(|compiled| compiled.rule.verified)
    }
    pub fn for_kind(kind: AgentKind) -> Result<Self> {
        Self::from_toml(match kind {
            AgentKind::Claude => include_str!("../rules/claude.toml"),
            AgentKind::Codex => include_str!("../rules/codex.toml"),
            AgentKind::Opencode => include_str!("../rules/opencode.toml"),
            AgentKind::Shell => include_str!("../rules/shell.toml"),
        })
    }
    pub fn observe(
        &self,
        lines: &[String],
        children: &[u32],
        exited: bool,
        pid: Option<u32>,
        started_at: &str,
    ) -> AgentInfo {
        let children: Vec<u32> = children
            .iter()
            .copied()
            .filter(|pid| !resident_process(*pid, &self.resident, &proc_entry))
            .collect();
        let children = children.as_slice();
        let mut info = AgentInfo {
            state: AgentState::Unknown,
            blocked_kind: None,
            interactive_ready: false,
            evidence: Evidence {
                rule: "unmatched".into(),
                text: lines
                    .iter()
                    .rev()
                    .take(12)
                    .rev()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n"),
            },
            pid,
            started_at: started_at.into(),
            observed_at: now(),
        };
        if exited {
            info.state = AgentState::Done;
            info.evidence = Evidence {
                rule: "process.exited".into(),
                text: "harness process exited".into(),
            };
            return info;
        }
        // Ordered contextual blockers win over busy markers. Matching never admits input.
        let tail = lines
            .iter()
            .rev()
            .take(12)
            .rev()
            .cloned()
            .collect::<Vec<_>>()
            .join(
                "
",
            );
        for compiled in &self.rules {
            let matched = if compiled.rule.multiline {
                (compiled.pattern.is_match(&tail)
                    && compiled
                        .requires
                        .iter()
                        .all(|pattern| pattern.is_match(&tail)))
                .then_some(tail.as_str())
            } else {
                lines
                    .iter()
                    .rev()
                    .take(12)
                    .find(|line| {
                        compiled.pattern.is_match(line)
                            && compiled
                                .requires
                                .iter()
                                .all(|pattern| pattern.is_match(line))
                    })
                    .map(String::as_str)
            };
            if let Some(line) = matched {
                if compiled.rule.state == AgentState::Idle && !children.is_empty() {
                    continue;
                }
                info.state = compiled.rule.state;
                info.blocked_kind = compiled.rule.blocked_kind;
                info.interactive_ready = compiled.rule.interactive_ready && children.is_empty();
                info.evidence = Evidence {
                    rule: compiled.rule.name.clone(),
                    text: line.trim().into(),
                };
                return info;
            }
        }
        if !children.is_empty() {
            info.state = AgentState::Working;
            info.evidence = Evidence {
                rule: "process.tool".into(),
                text: format!("running tool pids: {children:?}"),
            };
        }
        info
    }
}

/// `(command name and command line, parent pid)` of a process from `/proc/<pid>`.
fn proc_entry(pid: u32) -> Option<(String, u32)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
    let cmdline = String::from_utf8_lossy(&cmdline).replace('\0', " ");
    let open = stat.find('(')?;
    let close = stat.rfind(')')?;
    let name = format!("{} {cmdline}", stat.get(open + 1..close)?);
    let parent = stat
        .get(close + 2..)?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()?;
    Some((name, parent))
}

/// True when the process or one of its ancestors is a resident helper of the harness: an entry equals
/// its command name or appears in its command line.
fn resident_process(
    pid: u32,
    resident: &[String],
    entry: &dyn Fn(u32) -> Option<(String, u32)>,
) -> bool {
    if resident.is_empty() {
        return false;
    }
    let mut current = pid;
    for _ in 0..16 {
        let Some((name, parent)) = entry(current) else {
            return false;
        };
        let comm = name.split(' ').next().unwrap_or_default();
        if resident
            .iter()
            .any(|r| r == comm || name.contains(r.as_str()))
        {
            return true;
        }
        if parent <= 1 {
            return false;
        }
        current = parent;
    }
    false
}

#[cfg(test)]
mod resident_tests {
    use std::collections::HashMap;
    #[test]
    fn a_resident_helper_and_its_children_are_not_jobs() {
        let table: HashMap<u32, (&str, u32)> = HashMap::from([
            (10, ("codex", 1)),
            (11, ("node_repl", 10)),
            (12, ("node", 11)),
            (13, ("cargo", 10)),
            (
                14,
                (
                    "MainThread node /home/u/.codex/plugins/cache/x/launch.mjs",
                    10,
                ),
            ),
        ]);
        let entry = |pid: u32| table.get(&pid).map(|(n, p)| (n.to_string(), *p));
        let resident = vec!["node_repl".to_string(), "/.codex/plugins/".to_string()];
        assert!(super::resident_process(11, &resident, &entry));
        assert!(super::resident_process(12, &resident, &entry));
        assert!(!super::resident_process(13, &resident, &entry));
        assert!(!super::resident_process(13, &[], &entry));
        assert!(super::resident_process(14, &resident, &entry));
    }
}
