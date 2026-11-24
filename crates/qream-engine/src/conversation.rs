//! A pane's harness conversation read from its own session file into turns
//! `{at, who, text}`: Claude Code's project journal or Codex's rollout. Tool calls are
//! summarised in words; no file name or path is carried into a turn's chrome.
use crate::{Error, Result, storage::io};
use qream_term::agent::AgentKind;
use qream_term::agent::LaunchRecord;
use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Turn {
    pub at: String,
    pub who: String,
    pub text: String,
}

/// What a tool call did, in words.
fn tool_words(name: &str) -> &'static str {
    match name {
        "Bash" | "shell" | "exec_command" | "local_shell" | "local_shell_call" | "unified_exec" => {
            "ran a command"
        }
        "Read" | "read_file" | "view_image" => "read a file",
        "Edit" | "MultiEdit" | "apply_patch" | "NotebookEdit" => "edited a file",
        "Write" => "wrote a file",
        "Grep" | "Glob" | "LS" | "grep_files" | "list_dir" => "searched the files",
        "WebFetch" | "WebSearch" | "web_search" => "looked on the web",
        "Task" | "Agent" => "started a helper",
        "TodoWrite" | "update_plan" => "updated its plan",
        _ => "used a tool",
    }
}
fn texts(content: &Value) -> Vec<String> {
    match content {
        Value::String(t) => vec![t.clone()],
        Value::Array(items) => items
            .iter()
            .filter_map(|i| match i["type"].as_str() {
                Some("text" | "input_text" | "output_text") => {
                    i["text"].as_str().map(str::to_owned)
                }
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}
/// Harness scaffolding the operator never wrote: instructions and environment blocks.
fn scaffold(text: &str) -> bool {
    let t = text.trim_start();
    t.is_empty()
        || t.starts_with("<environment_context>")
        || t.starts_with("<user_instructions>")
        || t.starts_with("<system-reminder>")
        || t.starts_with("<command-")
        || t.starts_with("<local-command")
        || t.starts_with("# AGENTS.md instructions")
}
fn push(turns: &mut Vec<Turn>, at: &str, who: &str, text: String) {
    if !scaffold(&text) {
        turns.push(Turn {
            at: at.into(),
            who: who.into(),
            text,
        });
    }
}

/// Read a Claude Code `.jsonl` or a Codex `rollout-*.jsonl` into turns.
pub fn turns(path: &Path) -> Result<Vec<Turn>> {
    let data = io(path, std::fs::read_to_string(path))?;
    let mut turns = Vec::new();
    for line in data.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let at = v["timestamp"].as_str().unwrap_or("");
        match v["type"].as_str() {
            Some(kind @ ("user" | "assistant")) => {
                if v["isMeta"] == true || v["isSidechain"] == true {
                    continue;
                }
                let who = if kind == "user" { "operator" } else { "agent" };
                let content = &v["message"]["content"];
                for text in texts(content) {
                    push(&mut turns, at, who, text);
                }
                if let Value::Array(items) = content {
                    for item in items.iter().filter(|i| i["type"] == "tool_use") {
                        push(
                            &mut turns,
                            at,
                            "agent",
                            tool_words(item["name"].as_str().unwrap_or("")).into(),
                        );
                    }
                }
            }
            Some("response_item") => {
                let p = &v["payload"];
                match p["type"].as_str() {
                    Some("message") => {
                        let who = match p["role"].as_str() {
                            Some("user") => "operator",
                            Some("assistant") => "agent",
                            _ => continue,
                        };
                        for text in texts(&p["content"]) {
                            push(&mut turns, at, who, text);
                        }
                    }
                    Some("function_call" | "custom_tool_call" | "local_shell_call") => {
                        let name = p["name"].as_str().unwrap_or("local_shell");
                        push(&mut turns, at, "agent", tool_words(name).into());
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    Ok(turns)
}

/// Claude Code's project folder name for a working directory.
fn claude_project(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect()
}
fn modified_since(path: &Path, since: std::time::SystemTime) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .is_ok_and(|m| m >= since)
}
fn jsonl(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|d| {
            d.flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    files
}

/// The session file the pane's harness writes: the bound conversation when known, else
/// the sole one in the pane's folder written since the harness started. Several matching
/// journals cannot prove which history belongs to this pane.
pub fn find(
    home: &Path,
    kind: AgentKind,
    cwd: &Path,
    started_at: &str,
    conversation: Option<&str>,
) -> Result<Option<PathBuf>> {
    let started = chrono::DateTime::parse_from_rfc3339(started_at)
        .map_err(|_| Error::Denied("The harness start time is unreadable".into()))?;
    let since = std::time::SystemTime::from(started);
    Ok(match kind {
        AgentKind::Claude => {
            let dir = home.join(".claude/projects").join(claude_project(cwd));
            if let Some(id) = conversation {
                Some(dir.join(format!("{id}.jsonl"))).filter(|p| p.is_file())
            } else {
                unique(jsonl(&dir).into_iter().filter(|p| modified_since(p, since)))
            }
        }
        AgentKind::Codex => {
            let mut day = started.with_timezone(&chrono::Utc).date_naive();
            let today = chrono::Utc::now().date_naive();
            let mut matches = Vec::new();
            while day <= today {
                let dir = home
                    .join(".codex/sessions")
                    .join(day.format("%Y/%m/%d").to_string());
                matches.extend(jsonl(&dir).into_iter().filter(|p| {
                    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    if let Some(id) = conversation {
                        return name.ends_with(&format!("-{id}.jsonl"));
                    }
                    modified_since(p, since) && session_cwd(p).is_some_and(|c| c == cwd)
                }));
                day = day.succ_opt().unwrap_or(today + chrono::Days::new(1));
            }
            unique(matches.into_iter())
        }
        AgentKind::Opencode | AgentKind::Shell => None,
    })
}
fn unique(mut paths: impl Iterator<Item = PathBuf>) -> Option<PathBuf> {
    let first = paths.next()?;
    paths.next().is_none().then_some(first)
}
/// The working folder a Codex rollout records in its first line.
fn session_cwd(path: &Path) -> Option<PathBuf> {
    use std::io::BufRead;
    let file = std::fs::File::open(path).ok()?;
    let mut line = String::new();
    std::io::BufReader::new(file).read_line(&mut line).ok()?;
    let v: Value = serde_json::from_str(&line).ok()?;
    v["payload"]["cwd"].as_str().map(PathBuf::from)
}

/// Discover the native journal while the pane's process still supplies its working folder
/// and home. A stopped process cannot supply either, so control persists the id first.
pub(crate) fn identity(record: &LaunchRecord) -> Result<Option<String>> {
    if let Some(id) = record.conversation_id.as_ref().filter(|id| !id.is_empty()) {
        return Ok(Some(id.clone()));
    }
    // A harness explicitly launched into a known session is already bound, even if its
    // journal was created before this process. Session selectors are never inferred.
    if let Some(id) = record
        .spec
        .argv
        .windows(2)
        .find_map(|pair| match (record.spec.kind, pair[0].as_str()) {
            (AgentKind::Claude, "--session-id" | "--resume") | (AgentKind::Codex, "resume") => {
                Some(&pair[1])
            }
            _ => None,
        })
        .filter(|id| valid_id(id))
    {
        return Ok(Some(id.clone()));
    }
    let Some(pid) = record.pid else {
        return Ok(None);
    };
    let process = PathBuf::from(format!("/proc/{pid}"));
    let Ok(cwd) = std::fs::read_link(process.join("cwd")) else {
        return Ok(None);
    };
    let home = record.spec.env.get("HOME").map(PathBuf::from).or_else(|| {
        // Read only HOME from the pane's environment; no other variable leaves this function.
        let bytes = std::fs::read(process.join("environ")).ok()?;
        bytes.split(|b| *b == 0).find_map(|entry| {
            let value = entry.strip_prefix(b"HOME=")?;
            std::str::from_utf8(value).ok().map(PathBuf::from)
        })
    });
    let Some(home) = home else {
        return Ok(None);
    };
    let Some(file) = find(&home, record.spec.kind, &cwd, &record.started_at, None)? else {
        return Ok(None);
    };
    let id = match record.spec.kind {
        AgentKind::Claude => file.file_stem().and_then(|s| s.to_str()).map(str::to_owned),
        AgentKind::Codex => {
            use std::io::BufRead;
            let input = io(&file, std::fs::File::open(&file))?;
            let mut line = String::new();
            io(&file, std::io::BufReader::new(input).read_line(&mut line))?;
            serde_json::from_str::<Value>(&line).ok().and_then(|v| {
                (v["type"] == "session_meta")
                    .then(|| v["payload"]["id"].as_str().map(str::to_owned))
                    .flatten()
            })
        }
        AgentKind::Opencode | AgentKind::Shell => None,
    };
    Ok(id.filter(|id| valid_id(id)))
}
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn both_harness_journals_become_turns() {
        let dir = tempfile::tempdir().expect("scratch");
        let claude = dir.path().join("c.jsonl");
        std::fs::write(&claude, concat!(
            r#"{"type":"user","timestamp":"t1","message":{"role":"user","content":"Build the probe"}}"#, "\n",
            r#"{"type":"assistant","timestamp":"t2","message":{"content":[{"type":"text","text":"On it."},{"type":"tool_use","name":"Bash","input":{"command":"cargo test /secret/path"}}]}}"#, "\n",
            r#"{"type":"user","timestamp":"t3","message":{"content":[{"type":"tool_result","content":"ok"}]}}"#, "\n",
        )).expect("write");
        let got = turns(&claude).expect("turns");
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].who, "operator");
        assert_eq!(got[2].text, "ran a command");
        assert!(!got.iter().any(|t| t.text.contains("/secret")));
        let codex = dir.path().join("rollout.jsonl");
        std::fs::write(&codex, concat!(
            r#"{"timestamp":"t0","type":"session_meta","payload":{"cwd":"/w"}}"#, "\n",
            r#"{"timestamp":"t1","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>x</environment_context>"}]}}"#, "\n",
            r#"{"timestamp":"t2","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Seal it"}]}}"#, "\n",
            r#"{"timestamp":"t3","type":"response_item","payload":{"type":"function_call","name":"apply_patch","arguments":"{}"}}"#, "\n",
            r#"{"timestamp":"t4","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Sealed."}]}}"#, "\n",
        )).expect("write");
        let got = turns(&codex).expect("turns");
        let words: Vec<_> = got.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(words, ["Seal it", "edited a file", "Sealed."]);
        assert_eq!(session_cwd(&codex), Some(PathBuf::from("/w")));
    }
}
