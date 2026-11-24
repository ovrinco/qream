//! What QREAM started: process groups and transient user units recorded with their birth identity.
//! Nothing here signals a process unless its recorded identity still matches.
use crate::{
    Error, Result, now,
    storage::{read_json, write_json},
};
use nix::{
    errno::Errno,
    sys::signal::{Signal, killpg},
    unistd::Pid,
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant},
};

/// Every unit name QREAM gives a transient unit starts with this; a unit without it is never touched.
pub const UNIT_PREFIX: &str = "qream-";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Worker,
    Orchestrator,
    Controller,
    Job,
    Test,
    Viewer,
    Session,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Handle {
    /// A process group whose leader QREAM spawned; `leader_start` is the leader's start time in clock ticks.
    Group { pgid: i32, leader_start: u64 },
    /// A transient user unit QREAM created; `invocation` is its systemd invocation id at start.
    Unit { name: String, invocation: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Owned {
    pub id: String,
    pub campaign: String,
    pub root: PathBuf,
    pub task: Option<String>,
    pub wave: Option<u32>,
    pub role: Role,
    pub purpose: String,
    pub handle: Handle,
    pub started_at: String,
}

/// Who the record belongs to; shared by both constructors.
#[derive(Clone, Debug)]
pub struct Owner<'a> {
    pub campaign: &'a str,
    pub root: &'a Path,
    pub task: Option<&'a str>,
    pub wave: Option<u32>,
    pub role: Role,
    pub purpose: &'a str,
}

impl Owned {
    /// Records the group led by `pid`, which the caller has just spawned as a group leader.
    pub fn group(owner: &Owner, pid: u32) -> Result<Owned> {
        let stat = stat(pid).ok_or_else(|| Error::NotFound(format!("process {pid}")))?;
        let pgid = i32::try_from(pid).map_err(|_| Error::Invalid(format!("pid {pid}")))?;
        if stat.pgrp != pgid {
            return Err(Error::Invalid(format!(
                "process {pid} does not lead its own process group"
            )));
        }
        Ok(owner.with(
            format!("group:{pid}:{}", stat.start),
            Handle::Group {
                pgid,
                leader_start: stat.start,
            },
        ))
    }
    /// Records a transient user unit that QREAM has just created under its own prefix.
    pub fn unit(owner: &Owner, name: &str) -> Result<Owned> {
        if !name.starts_with(UNIT_PREFIX) {
            return Err(Error::NotOwned(format!(
                "unit {name} lacks the {UNIT_PREFIX} prefix"
            )));
        }
        let show = unit_show(name)?;
        if show.invocation.is_empty() {
            return Err(Error::NotFound(format!("unit {name}")));
        }
        Ok(owner.with(
            format!("unit:{name}:{}", show.invocation),
            Handle::Unit {
                name: name.into(),
                invocation: show.invocation,
            },
        ))
    }
}

impl Owner<'_> {
    fn with(&self, id: String, handle: Handle) -> Owned {
        Owned {
            id,
            campaign: self.campaign.into(),
            root: self.root.into(),
            task: self.task.map(Into::into),
            wave: self.wave,
            role: self.role,
            purpose: self.purpose.into(),
            handle,
            started_at: now(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Liveness {
    /// Still running with its recorded identity; the live pids (empty for a unit).
    Alive(Vec<u32>),
    Gone,
    /// The id now belongs to something QREAM did not start.
    Foreign(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Ended {
    Ended,
    AlreadyGone,
}

struct Stat {
    state: char,
    pgrp: i32,
    start: u64,
}

fn stat(pid: u32) -> Option<Stat> {
    let text = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = &text[text.rfind(')')? + 1..];
    let fields: Vec<&str> = rest.split_whitespace().collect();
    Some(Stat {
        state: fields.first()?.chars().next()?,
        pgrp: fields.get(2)?.parse().ok()?,
        start: fields.get(19)?.parse().ok()?,
    })
}

/// Live (non-zombie) members of a process group, with their start times.
fn members(pgid: i32) -> Vec<(u32, u64)> {
    let Ok(dir) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    dir.filter_map(|entry| entry.ok()?.file_name().to_str()?.parse::<u32>().ok())
        .filter_map(|pid| stat(pid).map(|s| (pid, s)))
        .filter(|(_, s)| s.pgrp == pgid && s.state != 'Z')
        .map(|(pid, s)| (pid, s.start))
        .collect()
}

struct UnitShow {
    active: String,
    invocation: String,
}

fn unit_show(name: &str) -> Result<UnitShow> {
    let out = Command::new("systemctl")
        .args([
            "--user",
            "show",
            "-p",
            "ActiveState",
            "-p",
            "InvocationID",
            name,
        ])
        .output()
        .map_err(|e| crate::error::io(Path::new("systemctl"), e))?;
    let text = String::from_utf8_lossy(&out.stdout);
    let field = |key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(key))
            .unwrap_or("")
            .trim()
            .to_string()
    };
    Ok(UnitShow {
        active: field("ActiveState="),
        invocation: field("InvocationID="),
    })
}

pub fn liveness(owned: &Owned) -> Result<Liveness> {
    match &owned.handle {
        Handle::Group { pgid, leader_start } => {
            let leader = u32::try_from(*pgid).ok().and_then(stat);
            if let Some(leader) = leader
                && leader.start != *leader_start
            {
                return Ok(Liveness::Foreign(format!(
                    "process group {pgid} now has another leader"
                )));
            }
            let live = members(*pgid);
            // A group id is reused only after every member is gone, so an older member means another group.
            if live.iter().any(|(_, start)| start < leader_start) {
                return Ok(Liveness::Foreign(format!(
                    "process group {pgid} holds a process older than its recorded leader"
                )));
            }
            Ok(if live.is_empty() {
                Liveness::Gone
            } else {
                Liveness::Alive(live.into_iter().map(|(pid, _)| pid).collect())
            })
        }
        Handle::Unit { name, invocation } => {
            let show = unit_show(name)?;
            let running = matches!(
                show.active.as_str(),
                "active" | "activating" | "deactivating" | "reloading"
            );
            if show.invocation.is_empty() || !running {
                Ok(Liveness::Gone)
            } else if &show.invocation != invocation {
                Ok(Liveness::Foreign(format!(
                    "unit {name} is another invocation"
                )))
            } else {
                Ok(Liveness::Alive(Vec::new()))
            }
        }
    }
}

/// Ends an owned group (terminate, then kill after `grace`) or stops an owned unit.
pub fn end(owned: &Owned, grace: Duration) -> Result<Ended> {
    match liveness(owned)? {
        Liveness::Gone => return Ok(Ended::AlreadyGone),
        Liveness::Foreign(why) => return Err(Error::NotOwned(why)),
        Liveness::Alive(_) => {}
    }
    match &owned.handle {
        Handle::Group { pgid, .. } => {
            signal(*pgid, Signal::SIGTERM)?;
            if wait_gone(owned, grace)? {
                return Ok(Ended::Ended);
            }
            // Identity is checked again: the group may have emptied and its id been reused meanwhile.
            match liveness(owned)? {
                Liveness::Alive(_) => signal(*pgid, Signal::SIGKILL)?,
                Liveness::Gone => return Ok(Ended::Ended),
                Liveness::Foreign(why) => return Err(Error::NotOwned(why)),
            }
            if wait_gone(owned, Duration::from_secs(5))? {
                Ok(Ended::Ended)
            } else {
                Err(Error::Signal(format!(
                    "process group {pgid} survived a kill"
                )))
            }
        }
        Handle::Unit { name, .. } => {
            let status = Command::new("systemctl")
                .args(["--user", "stop", name])
                .status()
                .map_err(|e| crate::error::io(Path::new("systemctl"), e))?;
            if !status.success() {
                return Err(Error::Signal(format!("unit {name} did not stop")));
            }
            if wait_gone(owned, grace.max(Duration::from_secs(5)))? {
                Ok(Ended::Ended)
            } else {
                Err(Error::Signal(format!("unit {name} is still active")))
            }
        }
    }
}

fn signal(pgid: i32, sig: Signal) -> Result<()> {
    match killpg(Pid::from_raw(pgid), sig) {
        Ok(()) | Err(Errno::ESRCH) => Ok(()),
        Err(e) => Err(Error::Signal(format!("{sig} to group {pgid}: {e}"))),
    }
}

fn wait_gone(owned: &Owned, limit: Duration) -> Result<bool> {
    let until = Instant::now() + limit;
    loop {
        match liveness(owned)? {
            Liveness::Gone => return Ok(true),
            Liveness::Foreign(why) => return Err(Error::NotOwned(why)),
            Liveness::Alive(_) if Instant::now() >= until => return Ok(false),
            Liveness::Alive(_) => thread::sleep(Duration::from_millis(50)),
        }
    }
}

/// The record of everything QREAM started, one JSON file for the user's home.
#[derive(Clone, Debug)]
pub struct Registry {
    path: PathBuf,
}

impl Registry {
    pub fn open(path: &Path) -> Registry {
        Registry { path: path.into() }
    }
    /// `$QREAM_HOME/state/owned.json`, the home defaulting to `~/.local/share/qream`.
    pub fn default_path() -> Result<PathBuf> {
        if std::env::var_os("QREAM_TEST").is_some_and(|v| v == "1") {
            return Ok(crate::daemon::Paths::from_env()?
                .home
                .join("state/owned.json"));
        }
        if let Some(home) = std::env::var_os("QREAM_HOME") {
            return Ok(PathBuf::from(home).join("state/owned.json"));
        }
        let home = std::env::var_os("HOME").ok_or_else(|| Error::NotFound("HOME".into()))?;
        Ok(PathBuf::from(home).join(".local/share/qream/state/owned.json"))
    }
    pub fn list(&self) -> Result<Vec<Owned>> {
        read_json(&self.path)
    }
    pub fn add(&self, owned: Owned) -> Result<()> {
        let mut all = self.list()?;
        all.retain(|o| o.id != owned.id);
        all.push(owned);
        write_json(&self.path, &all)
    }
    pub fn remove(&self, id: &str) -> Result<()> {
        let mut all = self.list()?;
        all.retain(|o| o.id != id);
        write_json(&self.path, &all)
    }
}
