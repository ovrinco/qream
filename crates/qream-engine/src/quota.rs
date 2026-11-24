//! Quota is account-bound. Admission reserves cost under a file lock; it never changes accounts.
use crate::{
    Error, Result,
    storage::{self, io},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Window {
    pub window_minutes: u64,
    pub used_percent: f64,
    pub resets_at: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountWindows {
    pub account: String,
    pub observed_at: i64,
    pub windows: Vec<Window>,
    pub limit_reached: bool,
}
pub trait WindowSource {
    fn accounts(&self) -> Result<Vec<AccountWindows>>;
}
/// Paths are assigned to an account by the caller's active-account configuration, never by credentials.
pub struct CodexSource {
    pub records: Vec<(String, PathBuf)>,
}
fn find_snapshot(v: &Value) -> Option<&Value> {
    if let Some(r) = v.get("rate_limits")
        && r["limit_id"].as_str().is_none_or(|s| s == "codex")
        && (r["primary"].is_object() || r["secondary"].is_object())
    {
        return Some(r);
    }
    match v {
        Value::Object(m) => m.values().find_map(find_snapshot),
        Value::Array(a) => a.iter().find_map(find_snapshot),
        _ => None,
    }
}
pub fn parse_snapshot(account: &str, observed_at: i64, v: &Value) -> Result<AccountWindows> {
    let r = find_snapshot(v).unwrap_or(v);
    let mut windows = vec![];
    for key in ["primary", "secondary"] {
        if r[key].is_null() {
            continue;
        }
        let w = &r[key];
        let used = w["used_percent"]
            .as_f64()
            .ok_or_else(|| Error::Denied("usage missing percentage".into()))?;
        let minutes = w["window_minutes"]
            .as_u64()
            .filter(|n| *n > 0)
            .ok_or_else(|| Error::Denied("usage missing duration".into()))?;
        let reset = w["resets_at"]
            .as_i64()
            .ok_or_else(|| Error::Denied("usage missing reset".into()))?;
        if !(0.0..=100.0).contains(&used) {
            return Err(Error::Denied("invalid usage percentage".into()));
        }
        windows.push(Window {
            window_minutes: minutes,
            used_percent: used,
            resets_at: reset,
        });
    }
    if windows.is_empty() {
        return Err(Error::Denied("no usage windows".into()));
    }
    Ok(AccountWindows {
        account: account.into(),
        observed_at,
        windows,
        limit_reached: r["rate_limit_reached_type"].as_str().is_some(),
    })
}
impl WindowSource for CodexSource {
    fn accounts(&self) -> Result<Vec<AccountWindows>> {
        let mut latest: BTreeMap<String, AccountWindows> = BTreeMap::new();
        for (account, path) in &self.records {
            let text = io(path, fs::read_to_string(path))?;
            let mut snap = None;
            for line in text.lines() {
                if let Ok(v) = serde_json::from_str::<Value>(line) {
                    if let Some(r) = find_snapshot(&v) {
                        snap = Some(r.clone());
                    } else if v["primary"].is_object() {
                        snap = Some(v);
                    }
                }
            }
            let s = snap
                .ok_or_else(|| Error::Denied(format!("no quota snapshot in {}", path.display())))?;
            let at = io(path, fs::metadata(path))?
                .modified()
                .map_err(|source| Error::Io {
                    path: path.clone(),
                    source,
                })?
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| Error::Denied("record clock before epoch".into()))?
                .as_secs() as i64;
            let a = parse_snapshot(account, at, &s)?;
            if latest.get(account).is_none_or(|old| old.observed_at <= at) {
                latest.insert(account.clone(), a);
            }
        }
        Ok(latest.into_values().collect())
    }
}
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct TaskCost {
    pub rolling_percent: f64,
    pub weekly_percent: f64,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum Admission {
    Admit {
        note: String,
    },
    Hold {
        reset_at: Option<i64>,
        reason: String,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Halt {
    reset_at: i64,
    printed_reset: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Reservation {
    account: String,
    cost: TaskCost,
}
pub struct Quota<'a> {
    pub root: &'a Path,
    pub source: &'a dyn WindowSource,
    pub active_account: &'a str,
    pub now: i64,
    pub max_age_secs: i64,
    pub rolling_line: f64,
    pub weekly_line: f64,
}
impl Quota<'_> {
    pub fn record_limit(&self, kind: &str, reset_at: i64, printed_reset: &str) -> Result<()> {
        storage::token(kind)?;
        let dir = self.root.join("state/quota");
        let _guard = storage::lock(&dir.join(".lock"))?;
        let path = dir.join(format!("{kind}.until"));
        let old: Option<Halt> = if path.exists() {
            Some(storage::read(&path)?)
        } else {
            None
        };
        // Later reset extends a halt; a shorter new line never releases starts prematurely.
        if old.is_none_or(|h| reset_at > h.reset_at) {
            storage::write(
                &path,
                &Halt {
                    reset_at,
                    printed_reset: printed_reset.into(),
                },
            )?;
        }
        Ok(())
    }
    pub fn admit(
        &self,
        kind: &str,
        next_task_cost: TaskCost,
        reservation_id: &str,
    ) -> Result<Admission> {
        storage::token(kind)?;
        storage::token(reservation_id)?;
        if !next_task_cost.rolling_percent.is_finite()
            || !next_task_cost.weekly_percent.is_finite()
            || next_task_cost.rolling_percent < 0.0
            || next_task_cost.weekly_percent < 0.0
        {
            return Err(Error::Denied("invalid task cost".into()));
        }
        let dir = self.root.join("state/quota");
        let _guard = storage::lock(&dir.join(".lock"))?;
        let halt = dir.join(format!("{kind}.until"));
        if halt.exists() {
            let h: Halt = storage::read(&halt)?;
            if self.now <= h.reset_at {
                return Ok(Admission::Hold {
                    reset_at: Some(h.reset_at),
                    reason: format!("printed usage reset: {}", h.printed_reset),
                });
            }
        }
        if kind == "claude" || kind == "opencode" {
            return Ok(Admission::Admit {
                note: "no record source".into(),
            });
        }
        if kind != "codex" {
            return Err(Error::Denied("unknown quota kind".into()));
        }
        let accounts = self.source.accounts()?;
        let Some(account) = accounts.iter().find(|a| a.account == self.active_account) else {
            return Ok(Admission::Hold {
                reset_at: None,
                reason: "active account has no usage record".into(),
            });
        };
        if self.now - account.observed_at > self.max_age_secs || account.observed_at > self.now + 60
        {
            return Ok(Admission::Hold {
                reset_at: None,
                reason: "usage snapshot is stale or in the future".into(),
            });
        }
        let path = dir.join("reservations.json");
        let mut reservations: BTreeMap<String, Reservation> = if path.exists() {
            storage::read(&path)?
        } else {
            BTreeMap::new()
        };
        if reservations.contains_key(reservation_id) {
            return Err(Error::Denied("task already reserved".into()));
        }
        let mut holds = vec![];
        for w in &account.windows {
            let rolling = w.window_minutes <= 600;
            let cost = if rolling {
                next_task_cost.rolling_percent
            } else {
                next_task_cost.weekly_percent
            };
            let pending: f64 = reservations
                .values()
                .filter(|r| r.account == account.account)
                .map(|r| {
                    if rolling {
                        r.cost.rolling_percent
                    } else {
                        r.cost.weekly_percent
                    }
                })
                .sum();
            let used = if self.now > w.resets_at {
                0.0
            } else {
                w.used_percent
            };
            let line = if rolling {
                self.rolling_line
            } else {
                self.weekly_line
            };
            if used + pending + cost >= line || (account.limit_reached && self.now <= w.resets_at) {
                holds.push(w.resets_at);
            }
        }
        if let Some(reset) = holds.into_iter().max() {
            return Ok(Admission::Hold {
                reset_at: Some(reset),
                reason: "window lacks reserved headroom for next task".into(),
            });
        }
        reservations.insert(
            reservation_id.into(),
            Reservation {
                account: account.account.clone(),
                cost: next_task_cost,
            },
        );
        storage::write(&path, &reservations)?;
        Ok(Admission::Admit {
            note: format!("reserved headroom on {}", account.account),
        })
    }
    /// Release only after the controller reconciles this task's usage into a fresh snapshot.
    pub fn settle(&self, id: &str) -> Result<()> {
        let dir = self.root.join("state/quota");
        let _guard = storage::lock(&dir.join(".lock"))?;
        let path = dir.join("reservations.json");
        let mut r: BTreeMap<String, Reservation> = if path.exists() {
            storage::read(&path)?
        } else {
            BTreeMap::new()
        };
        r.remove(id);
        storage::write(&path, &r)
    }
}
impl Quota<'_> {
    /// Provider ceiling for the active account after pending reservations, without reserving a start.
    pub fn provider_slots(&self, cost: TaskCost, session_cap: u64) -> Result<u64> {
        let dir = self.root.join("state/quota");
        let _guard = storage::lock(&dir.join(".lock"))?;
        let halt = dir.join("codex.until");
        if halt.exists() {
            let h: Halt = storage::read(&halt)?;
            if self.now <= h.reset_at {
                return Ok(0);
            }
        }
        let accounts = self.source.accounts()?;
        let Some(account) = accounts.iter().find(|a| a.account == self.active_account) else {
            return Ok(0);
        };
        if self.now - account.observed_at > self.max_age_secs || account.observed_at > self.now + 60
        {
            return Ok(0);
        }
        let path = dir.join("reservations.json");
        let reservations: BTreeMap<String, Reservation> = if path.exists() {
            storage::read(&path)?
        } else {
            BTreeMap::new()
        };
        let mut slots = session_cap;
        for w in &account.windows {
            if account.limit_reached && self.now <= w.resets_at {
                return Ok(0);
            }
            let rolling = w.window_minutes <= 600;
            let need = if rolling {
                cost.rolling_percent
            } else {
                cost.weekly_percent
            };
            if !need.is_finite() || need < 0.0 {
                return Err(Error::Denied("invalid provider cost".into()));
            }
            let pending: f64 = reservations
                .values()
                .filter(|r| r.account == account.account)
                .map(|r| {
                    if rolling {
                        r.cost.rolling_percent
                    } else {
                        r.cost.weekly_percent
                    }
                })
                .sum();
            let used = if self.now > w.resets_at {
                0.0
            } else {
                w.used_percent
            };
            let headroom = (if rolling {
                self.rolling_line
            } else {
                self.weekly_line
            }) - used
                - pending;
            if headroom <= 0.0 {
                return Ok(0);
            }
            if need > 0.0 {
                slots = slots.min((headroom / need).ceil().max(1.0) as u64 - 1);
            }
        }
        Ok(slots)
    }
}
