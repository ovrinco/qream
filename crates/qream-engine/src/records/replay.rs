//! The replay recording: one event per tool call in the harnesses' own session files, written to
//! `state/replay/<task>.jsonl` for the Review page. Reading files only; no model is called.
//!
//! Normalised tool calls: a Codex `response_item` whose payload type is `custom_tool_call` or
//! `function_call`, and each `tool_use` block of a Claude Code assistant message.
use super::json_lines;
use crate::{Error, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

/// A session file bound to a task. `task` "*" attributes each line by the `work/TNN/` of its cwd.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Binding {
    pub task: String,
    pub harness: String,
    pub path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub at: String,
    pub task: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    pub tool: String,
    pub source: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Summary {
    pub task: String,
    pub counts: BTreeMap<String, usize>,
    pub first: String,
    pub last: String,
}

static TASK_DIR: LazyLock<Regex> = LazyLock::new(|| re(r"/work/(T\d+)(/|$)"));
static TASK_NAME: LazyLock<Regex> = LazyLock::new(|| re(r"^T\d+$"));
static PATCH_FILE: LazyLock<Regex> =
    LazyLock::new(|| re(r"\*\*\* (?:Update|Add|Delete) File: (\S+)"));
static CMD: LazyLock<Regex> = LazyLock::new(|| re(r#"cmd"?\s*:\s*"((?:[^"\\]|\\.)*)""#));
static ABS_PATH: LazyLock<Regex> =
    LazyLock::new(|| re(r#"(/[A-Za-z0-9._\-]+(?:/[A-Za-z0-9._\-]+)+)"#));
static WRITE_CMD: LazyLock<Regex> = LazyLock::new(|| {
    re(r"write_text\(|write_bytes\(|\bcat\s*>>?|\btee\b|\bcp\b|\bmv\b|apply_patch")
});
static READ_CMD: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^\s*(cat|sed -n|rg|grep|head|tail|ls|wc|find|git (diff|status|show|log))\b")
});
static MESSAGE_CMD: LazyLock<Regex> =
    LazyLock::new(|| re(r"\bqream (send|msg|submit|ask|answer|broadcast)\b"));

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("the patterns above are fixed and valid")
}

fn replay_dir(root: &Path) -> PathBuf {
    root.join("state/replay")
}

/// Binds the campaign's session files: Codex rollouts whose first line's cwd is under
/// `<root>/work/TNN/`, and the Claude Code project folder of the campaign root (task "*").
pub fn discover(
    root: &Path,
    codex_sessions: &Path,
    claude_projects: &Path,
) -> Result<Vec<Binding>> {
    let root_s = root.to_string_lossy().into_owned();
    let mut out = Vec::new();
    let mut stack = vec![codex_sessions.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "jsonl") {
                let first = first_line(&p)?;
                let cwd = first["payload"]["cwd"].as_str().unwrap_or_default();
                if let Some(rest) = cwd.strip_prefix(&root_s)
                    && let Some(c) = TASK_DIR.captures(rest)
                {
                    out.push(Binding {
                        task: c[1].into(),
                        harness: "codex".into(),
                        path: p,
                    });
                }
            }
        }
    }
    let project = claude_projects.join(root_s.replace(['/', '_', '.'], "-"));
    if let Ok(entries) = std::fs::read_dir(&project) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_file() && p.extension().is_some_and(|x| x == "jsonl") {
                out.push(Binding {
                    task: "*".into(),
                    harness: "claude".into(),
                    path: p,
                });
            }
        }
    }
    // Each worker's own Claude Code project folder, bound to its task at its start.
    if let Ok(work) = std::fs::read_dir(root.join("work")) {
        for task in work.flatten() {
            let name = task.file_name().to_string_lossy().into_owned();
            if !TASK_NAME.is_match(&name) {
                continue;
            }
            for cwd in [task.path().join("candidate"), task.path()] {
                let folder =
                    claude_projects.join(cwd.to_string_lossy().replace(['/', '_', '.'], "-"));
                let Ok(entries) = std::fs::read_dir(&folder) else {
                    continue;
                };
                for e in entries.flatten() {
                    let p = e.path();
                    if p.is_file() && p.extension().is_some_and(|x| x == "jsonl") {
                        out.push(Binding {
                            task: name.clone(),
                            harness: "claude".into(),
                            path: p,
                        });
                    }
                }
            }
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

/// The harnesses' session folders: `QREAM_CODEX_SESSIONS` and `QREAM_CLAUDE_PROJECTS` when
/// set (tests), else `~/.codex/sessions` and `~/.claude/projects`.
pub fn sources() -> Result<(PathBuf, PathBuf)> {
    let var = |key: &str| std::env::var_os(key).filter(|v| !v.is_empty());
    let home = var("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| Error::Denied("HOME is not set".into()))?;
    Ok((
        var("QREAM_CODEX_SESSIONS").map_or_else(|| home.join(".codex/sessions"), PathBuf::from),
        var("QREAM_CLAUDE_PROJECTS").map_or_else(|| home.join(".claude/projects"), PathBuf::from),
    ))
}

fn first_line(p: &Path) -> Result<Value> {
    use std::io::BufRead;
    let f = std::fs::File::open(p).map_err(|e| Error::io(p, e))?;
    let mut line = String::new();
    std::io::BufReader::new(f)
        .read_line(&mut line)
        .map_err(|e| Error::io(p, e))?;
    Ok(serde_json::from_str(&line).unwrap_or(Value::Null))
}

/// The events of one session file, one per normalised tool call, in file order.
pub fn normalise(b: &Binding) -> Result<Vec<Event>> {
    let mut out = Vec::new();
    let file = b.path.to_string_lossy();
    for (line, v) in json_lines(&b.path)? {
        let source = format!("{file}:{line}");
        let at = v["timestamp"].as_str().unwrap_or_default().to_string();
        let task = if b.task == "*" {
            v["cwd"]
                .as_str()
                .and_then(|c| TASK_DIR.captures(c).map(|c| c[1].to_string()))
                .unwrap_or_else(|| "orchestrator".into())
        } else {
            b.task.clone()
        };
        let p = &v["payload"];
        if v["type"] == "response_item"
            && matches!(
                p["type"].as_str(),
                Some("custom_tool_call" | "function_call")
            )
        {
            let input = match (&p["input"], &p["arguments"]) {
                (Value::String(s), _) | (_, Value::String(s)) => s.clone(),
                _ => String::new(),
            };
            let (kind, path, bytes) = classify_codex(&input);
            out.push(Event {
                at,
                task,
                kind,
                path,
                bytes,
                tool: p["name"].as_str().unwrap_or_default().into(),
                source,
            });
        } else if v["type"] == "assistant"
            && let Some(blocks) = v["message"]["content"].as_array()
        {
            for c in blocks.iter().filter(|c| c["type"] == "tool_use") {
                let (kind, path, bytes) = classify_claude(c);
                out.push(Event {
                    at: at.clone(),
                    task: task.clone(),
                    kind,
                    path,
                    bytes,
                    tool: c["name"].as_str().unwrap_or_default().into(),
                    source: source.clone(),
                });
            }
        }
    }
    Ok(out)
}

type Class = (String, Option<String>, Option<u64>);

fn classify_codex(input: &str) -> Class {
    if let Some(c) = PATCH_FILE.captures(input) {
        return ("write".into(), Some(c[1].into()), Some(input.len() as u64));
    }
    let cmd = CMD
        .captures(input)
        .map(|c| c[1].to_string())
        .unwrap_or_else(|| input.to_string());
    let path = ABS_PATH.captures(&cmd).map(|c| c[1].to_string());
    if WRITE_CMD.is_match(&cmd) {
        ("write".into(), path, Some(cmd.len() as u64))
    } else if MESSAGE_CMD.is_match(&cmd) {
        ("message".into(), path, None)
    } else if READ_CMD.is_match(&cmd) {
        ("read".into(), path, None)
    } else {
        ("run".into(), path, None)
    }
}

fn classify_claude(block: &Value) -> Class {
    let i = &block["input"];
    let s = |k: &str| i[k].as_str().map(String::from);
    let len = |k: &str| i[k].as_str().map(|t| t.len() as u64);
    match block["name"].as_str().unwrap_or_default() {
        "Read" | "Glob" | "Grep" => ("read".into(), s("file_path").or(s("path")), None),
        "Write" => ("write".into(), s("file_path"), len("content")),
        "Edit" | "NotebookEdit" => (
            "write".into(),
            s("file_path").or(s("notebook_path")),
            len("new_string").or(len("new_source")),
        ),
        "SendMessage" | "Agent" | "AskUserQuestion" => ("message".into(), None, None),
        "Bash" => classify_codex(&format!(
            "cmd:{}",
            serde_json::to_string(&i["command"]).unwrap_or_default()
        )),
        _ => ("run".into(), None, None),
    }
}

/// Writes `state/replay/<task>.jsonl` for every task of the bindings, replacing earlier
/// recordings whole so a re-run gives the same files. Returns the events per task.
pub fn record(root: &Path, bindings: &[Binding]) -> Result<BTreeMap<String, usize>> {
    // A test may inspect a real campaign through a private daemon, but its recorder
    // must never write into that campaign. The acceptance runner supplies this root.
    if std::env::var_os("QREAM_TEST").is_some_and(|value| value == "1") {
        let scratch = std::env::var_os("QREAM_TEST_SCRATCH")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let scratch = std::fs::canonicalize(&scratch).map_err(|e| Error::io(&scratch, e))?;
        let resolved = std::fs::canonicalize(root).map_err(|e| Error::io(root, e))?;
        if !resolved.starts_with(&scratch) {
            return Err(Error::Denied(format!(
                "Test replay refuses writes outside private scratch: {}",
                root.display()
            )));
        }
    }

    let mut by_task: BTreeMap<String, Vec<Event>> = BTreeMap::new();
    for b in bindings {
        for e in normalise(b)? {
            by_task.entry(e.task.clone()).or_default().push(e);
        }
    }
    let dir = replay_dir(root);
    std::fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
    let mut counts = BTreeMap::new();
    for (task, mut events) in by_task {
        events.sort_by(|a, b| a.at.cmp(&b.at));
        let mut body = String::new();
        for e in &mut events {
            e.path = e
                .path
                .as_deref()
                .map(|p| crate::mask::Mask::house().apply(p));
            body.push_str(&serde_json::to_string(e)?);
            body.push('\n');
        }
        let file = dir.join(format!("{task}.jsonl"));
        let tmp = dir.join(format!(".{task}.jsonl.tmp"));
        std::fs::write(&tmp, body).map_err(|e| Error::io(&tmp, e))?;
        std::fs::rename(&tmp, &file).map_err(|e| Error::io(&file, e))?;
        counts.insert(task, events.len());
    }
    let bfile = dir.join("bindings.json");
    std::fs::write(&bfile, serde_json::to_vec_pretty(bindings)?)
        .map_err(|e| Error::io(&bfile, e))?;
    Ok(counts)
}

/// The tasks that have a recording.
pub fn list(root: &Path) -> Result<Vec<String>> {
    let dir = replay_dir(root);
    let mut out: Vec<String> = match std::fs::read_dir(&dir) {
        Ok(d) => d
            .flatten()
            .filter_map(|e| {
                let n = e.file_name().to_string_lossy().into_owned();
                n.strip_suffix(".jsonl")
                    .filter(|t| !t.starts_with('.'))
                    .map(String::from)
            })
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(Error::io(&dir, e)),
    };
    out.sort();
    Ok(out)
}

/// Recorded events, optionally of one task and between two RFC 3339 times (inclusive).
pub fn events(
    root: &Path,
    task: Option<&str>,
    from: Option<&str>,
    to: Option<&str>,
) -> Result<Vec<Event>> {
    let tasks = match task {
        Some(t) => vec![t.to_string()],
        None => list(root)?,
    };
    let mut out = Vec::new();
    for t in tasks {
        for (_, v) in json_lines(&replay_dir(root).join(format!("{t}.jsonl")))? {
            let e: Event = serde_json::from_value(v)?;
            if from.is_none_or(|f| e.at.as_str() >= f) && to.is_none_or(|x| e.at.as_str() <= x) {
                out.push(e);
            }
        }
    }
    out.sort_by(|a, b| a.at.cmp(&b.at));
    Ok(out)
}

/// Counts per task and kind, with the first and last time.
pub fn summary(root: &Path) -> Result<Vec<Summary>> {
    let mut out = Vec::new();
    for task in list(root)? {
        let events = events(root, Some(&task), None, None)?;
        let mut s = Summary {
            task,
            ..Summary::default()
        };
        for e in &events {
            *s.counts.entry(e.kind.clone()).or_default() += 1;
        }
        s.first = events.first().map(|e| e.at.clone()).unwrap_or_default();
        s.last = events.last().map(|e| e.at.clone()).unwrap_or_default();
        out.push(s);
    }
    Ok(out)
}
