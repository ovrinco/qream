//! Usage per role and task: tokens from the harness's own session file, CPU and wall time from
//! the pane process's accounting, written to the store's `usage` table.
use super::json_lines;
use crate::{Error, Result};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;
use tracker_store::{Store, Usage};

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Tokens {
    pub input: u64,
    pub cached: u64,
    pub output: u64,
    /// A subset of output, never added to output a second time.
    pub reasoning: u64,
}

impl Tokens {
    fn add(&mut self, other: &Self) -> Result<()> {
        let sum = |a: u64, b: u64| {
            a.checked_add(b)
                .ok_or_else(|| Error::Denied("session token count exceeds u64".into()))
        };
        self.input = sum(self.input, other.input)?;
        self.cached = sum(self.cached, other.cached)?;
        self.output = sum(self.output, other.output)?;
        self.reasoning = sum(self.reasoning, other.reasoning)?;
        Ok(())
    }
}

/// Codex's last cumulative token_count and Claude's final usage for each message id.
/// Older token_usage_record deltas remain supported when no cumulative event is present.
pub fn tokens(session: &Path) -> Result<Tokens> {
    let mut legacy = Tokens::default();
    let mut cumulative = None;
    let mut messages = BTreeMap::new();
    for (_, v) in json_lines(session)? {
        let n = |u: &Value, k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
        let codex = |u: &Value| Tokens {
            input: n(u, "input_tokens"),
            cached: n(u, "cached_input_tokens"),
            output: n(u, "output_tokens"),
            reasoning: n(u, "reasoning_output_tokens"),
        };
        if v["type"] == "event_msg" && v["payload"]["type"] == "token_count" {
            let u = &v["payload"]["info"]["total_token_usage"];
            if u.is_object() {
                cumulative = Some(codex(u));
            }
        } else if v["type"] == "token_usage_record" {
            let u = &v["payload"]["usage"];
            legacy.add(&codex(u))?;
        } else if v["type"] == "assistant" {
            let m = &v["message"];
            let id = m["id"].as_str().unwrap_or_default().to_string();
            if id.is_empty() || !m["usage"].is_object() {
                continue;
            }
            let u = &m["usage"];
            let input = n(u, "input_tokens")
                .checked_add(n(u, "cache_creation_input_tokens"))
                .ok_or_else(|| Error::Denied("session token count exceeds u64".into()))?;
            messages.insert(
                id,
                Tokens {
                    input,
                    cached: n(u, "cache_read_input_tokens"),
                    output: n(u, "output_tokens"),
                    reasoning: n(&u["output_tokens_details"], "thinking_tokens"),
                },
            );
        }
    }
    let mut total = cumulative.unwrap_or(legacy);
    for message in messages.values() {
        total.add(message)?;
    }
    Ok(total)
}

/// Linux reports process times in USER_HZ, which is 100 on every architecture we run.
const USER_HZ: f64 = 100.0;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ProcessTime {
    pub cpu_s: f64,
    pub wall_s: f64,
}

/// CPU time of `pid` and its waited-for children, and its wall time since start.
pub fn process_time(pid: u32) -> Result<ProcessTime> {
    let path = Path::new("/proc").join(pid.to_string()).join("stat");
    let stat = std::fs::read_to_string(&path).map_err(|e| Error::io(&path, e))?;
    let uptime_path = Path::new("/proc/uptime");
    let uptime = std::fs::read_to_string(uptime_path).map_err(|e| Error::io(uptime_path, e))?;
    // Fields after the command name, which may hold spaces, start at state (field 3).
    let rest = stat
        .rsplit_once(')')
        .map(|(_, r)| r)
        .ok_or_else(|| Error::Denied(format!("unreadable {}", path.display())))?;
    let f: Vec<&str> = rest.split_whitespace().collect();
    let field = |n: usize| -> Result<f64> {
        f.get(n - 3)
            .and_then(|s| s.parse::<f64>().ok())
            .ok_or_else(|| Error::Denied(format!("{} lacks field {n}", path.display())))
    };
    let ticks = field(14)? + field(15)? + field(16)? + field(17)?;
    let up: f64 = uptime
        .split_whitespace()
        .next()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| Error::Denied("unreadable /proc/uptime".into()))?;
    Ok(ProcessTime {
        cpu_s: ticks / USER_HZ,
        wall_s: (up - field(22)? / USER_HZ).max(0.0),
    })
}

pub struct Record<'a> {
    pub campaign: &'a str,
    pub role: &'a str,
    pub task: &'a str,
    pub run: &'a str,
    pub tokens: &'a Tokens,
    pub time: Option<&'a ProcessTime>,
    pub outcome: &'a str,
}

/// One usage row in the store; returns its sequence number.
pub fn record(store: &Store, r: &Record) -> Result<i64> {
    let mut u = Usage::new();
    u.insert("campaign".into(), json!(r.campaign));
    u.insert("role".into(), json!(r.role));
    u.insert("unit".into(), json!(r.task));
    u.insert("run".into(), json!(r.run));
    u.insert("tokens_in".into(), json!(r.tokens.input));
    u.insert("tokens_cached".into(), json!(r.tokens.cached));
    u.insert("tokens_out".into(), json!(r.tokens.output));
    u.insert(
        "detail".into(),
        json!(json!({"reasoning_tokens":r.tokens.reasoning}).to_string()),
    );
    if let Some(t) = r.time {
        u.insert("cpu_s".into(), json!(t.cpu_s));
        u.insert("wall_s".into(), json!(t.wall_s));
    }
    u.insert("outcome".into(), json!(r.outcome));
    Ok(store.usage_record(&u)?)
}
