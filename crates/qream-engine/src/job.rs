//! Task-owned transient units. The monitor and stop hook both finish durably and idempotently.
use crate::{
    Error, Result,
    capacity::{Capacity, thermal_admit},
    mail::Mail,
    records::duration,
    storage::{self, io},
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub campaign: String,
    pub task: String,
    pub name: String,
    pub unit: String,
    pub cwd: PathBuf,
    pub command: Vec<String>,
    pub log: PathBuf,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub exit_code: Option<i32>,
    pub memory_max: u64,
    pub cpu_quota_percent: u32,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskInput {
    pub waiting_on_job: Vec<String>,
}
pub struct Start<'a> {
    pub root: &'a Path,
    pub campaign: &'a str,
    pub task: &'a str,
    pub name: &'a str,
    pub cwd: &'a Path,
    pub command: &'a [String],
    pub monitor: &'a Path,
    pub capacity: &'a Capacity,
    pub celsius: Option<f64>,
    pub memory_max: u64,
    pub cpu_quota_percent: u32,
}
fn path(root: &Path, task: &str, name: &str) -> Result<PathBuf> {
    storage::token(task)?;
    storage::token(name)?;
    Ok(root.join(format!("state/jobs/{task}/{name}.json")))
}
fn record(root: &Path, job: &Job) -> Result<()> {
    storage::write(&path(root, &job.task, &job.name)?, job)?;
    storage::append(&root.join("state/jobs.jsonl"), job)?;
    let active = list(root, &job.task)?
        .into_iter()
        .filter(|j| j.ended_at.is_none())
        .map(|j| j.name)
        .collect();
    storage::write(
        &root.join(format!("state/tasks/{}.jobs.json", job.task)),
        &TaskInput {
            waiting_on_job: active,
        },
    )
}
pub fn list(root: &Path, task: &str) -> Result<Vec<Job>> {
    storage::token(task)?;
    let dir = root.join(format!("state/jobs/{task}"));
    if !dir.exists() {
        return Ok(vec![]);
    }
    let mut paths = io(&dir, fs::read_dir(&dir))?
        .map(|e| io(&dir, e).map(|e| e.path()))
        .collect::<Result<Vec<_>>>()?;
    paths.sort();
    paths
        .into_iter()
        .filter(|p| p.extension().is_some_and(|s| s == "json"))
        .map(|p| storage::read(&p))
        .collect()
}
fn quote(s: &str) -> String {
    format!(
        "\"{}\"",
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('$', "$$")
    )
}
pub fn unit_args(s: &Start<'_>, unit: &str) -> Vec<String> {
    vec![
        "--user".into(),
        "--no-ask-password".into(),
        "--quiet".into(),
        format!("--unit={unit}"),
        "--property=AllowedCPUs=3-6".into(),
        "--property=CPUAffinity=3 4 5 6".into(),
        "--property=Nice=19".into(),
        "--property=IOSchedulingClass=idle".into(),
        format!("--property=CPUQuota={}%", s.cpu_quota_percent),
        format!("--property=MemoryMax={}", s.memory_max),
        "--property=KillMode=control-group".into(),
        format!(
            "--property=ExecStopPost={} finish {} {} {} $EXIT_STATUS",
            quote(&s.monitor.to_string_lossy()),
            quote(&s.root.to_string_lossy()),
            quote(s.task),
            quote(s.name)
        ),
        s.monitor.to_string_lossy().into(),
        "run".into(),
        s.root.to_string_lossy().into(),
        s.task.into(),
        s.name.into(),
    ]
}
/// The controller performs quota/runtime/input admission before calling this method.
/// Capacity and heat are rechecked here; enforceable limits are mandatory.
pub fn start(s: &Start<'_>) -> Result<Job> {
    storage::token(s.campaign)?;
    storage::token(s.task)?;
    storage::token(s.name)?;
    s.capacity.admit()?;
    thermal_admit(s.celsius)?;
    if s.memory_max > s.capacity.memory_per_task
        || s.memory_max == 0
        || s.cpu_quota_percent == 0
        || s.cpu_quota_percent > 400
        || !s.cwd.is_dir()
        || !s.monitor.is_file()
        || s.command.is_empty()
    {
        return Err(Error::Denied(
            "job needs valid cwd, monitor, command and constrained limits".into(),
        ));
    }
    if s.root.join("STOP").exists() {
        return Err(Error::Denied("campaign STOP".into()));
    }
    let _guard = storage::lock(&s.root.join("state/.jobs.lock"))?;
    let p = path(s.root, s.task, s.name)?;
    if p.exists() {
        return Err(Error::Denied("job name already owned by task".into()));
    }
    // A supplied ceiling admits only that many concurrent jobs even before /proc catches up.
    let all = io(
        &s.root.join("state/jobs"),
        fs::read_dir(s.root.join("state/jobs")),
    );
    let mut active = 0;
    if let Ok(dirs) = all {
        for dir in dirs {
            let dir = io(&s.root.join("state/jobs"), dir)?;
            if let Some(task) = dir.file_name().to_str() {
                active += list(s.root, task)?
                    .iter()
                    .filter(|j| j.ended_at.is_none())
                    .count() as u64;
            }
        }
    }
    if active >= s.capacity.slots {
        return Err(Error::Denied(format!(
            "capacity held by {}",
            s.capacity.binding
        )));
    }
    let unit = format!("qream-job-{}-{}-{}", s.campaign, s.task, s.name);
    let log = s.root.join(format!("state/jobs/{}/{}.log", s.task, s.name));
    let j = Job {
        campaign: s.campaign.into(),
        task: s.task.into(),
        name: s.name.into(),
        unit: unit.clone(),
        cwd: s.cwd.into(),
        command: s.command.to_vec(),
        log,
        started_at: storage::now(),
        ended_at: None,
        exit_code: None,
        memory_max: s.memory_max,
        cpu_quota_percent: s.cpu_quota_percent,
    };
    // The separate monitor must use the same store even after a daemon restart.
    let binding = s
        .root
        .join(format!("state/jobs/{}/{}.store", s.task, s.name));
    let store = crate::daemon::Paths::from_env()?.store;
    storage::write(&binding, &store)?;
    record(s.root, &j)?;
    let output = io(
        s.monitor,
        Command::new("systemd-run")
            .args(unit_args(s, &unit))
            .output(),
    )?;
    if !output.status.success() {
        finish_unlocked(s.root, s.task, s.name, 125)?;
        return Err(Error::Denied(format!(
            "job unit launch failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(j)
}
/// Called inside the transient unit; child shells stay within its memory and CPU cgroup.
pub fn run(root: &Path, task: &str, name: &str) -> Result<i32> {
    let mut j: Job = storage::read(&path(root, task, name)?)?;
    // An older caller's explicit "now" means the observed entry into the runner.
    if j.started_at == "now" {
        j.started_at = storage::now();
        record(root, &j)?;
    }
    let f = io(
        &j.log,
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&j.log),
    )?;
    let stderr = io(&j.log, f.try_clone())?;
    let status = io(
        &j.cwd,
        Command::new("taskset")
            .args(["-c", "3-6", "nice", "-n", "19", "ionice", "-c3"])
            .args(&j.command)
            .current_dir(&j.cwd)
            .stdin(Stdio::null())
            .stdout(f)
            .stderr(stderr)
            .status(),
    );
    let code = status.map(|s| s.code().unwrap_or(128)).unwrap_or(127);
    finish(root, task, name, code)?;
    Ok(code)
}
fn finish_unlocked(root: &Path, task: &str, name: &str, exit_code: i32) -> Result<Job> {
    let mut j: Job = storage::read(&path(root, task, name)?)?;
    if j.ended_at.is_none() {
        j.ended_at = Some(storage::now());
        j.exit_code = Some(exit_code);
        record_duration(root, &j)?;
        record(root, &j)?;
    } else {
        // Repair a partial completion; the store row is idempotent under the job lock.
        record_duration(root, &j)?;
    }
    let id = format!("job-{task}-{name}-ended");
    let mail = Mail::new(root);
    let to = format!("{}/worker/{task}", j.campaign);
    if mail.get(&to, &id).is_err() {
        mail.send(
            &id,
            &format!("{}/controller", j.campaign),
            &to,
            &serde_json::to_string(&j)?,
            false,
        )?;
    }
    Ok(j)
}

/// Name first, then command words: tests/builds keep their own kind; other jobs are task work.
pub fn kind(job: &Job) -> &'static str {
    let classify = |words: &str| {
        let words: Vec<_> = words.split(|c: char| !c.is_ascii_alphanumeric()).collect();
        if words
            .iter()
            .any(|w| matches!(*w, "test" | "tests" | "pytest" | "ctest"))
        {
            Some("test")
        } else if words
            .iter()
            .any(|w| matches!(*w, "build" | "compile" | "make" | "cmake" | "rustc"))
        {
            Some("build")
        } else if words.contains(&"check") {
            Some("check")
        } else if words.contains(&"install") {
            Some("install")
        } else {
            None
        }
    };
    classify(&job.name.to_ascii_lowercase())
        .or_else(|| classify(&job.command.join(" ").to_ascii_lowercase()))
        .unwrap_or("task")
}

fn record_duration(root: &Path, j: &Job) -> Result<()> {
    let per_job = root.join(format!("state/jobs/{}/{}.store", j.task, j.name));
    let binding = if per_job.is_file() {
        per_job.clone()
    } else {
        root.join("state/records-store.json")
    };
    let path: PathBuf = if binding.is_file() {
        storage::read(&binding)?
    } else {
        // Older jobs inherit the monitor's configured store, then retain that identity for retry.
        let path = crate::daemon::Paths::from_env()?.store;
        storage::write(&per_job, &path)?;
        path
    };
    let store = tracker_store::Store::open(&path, "qream-job-monitor")?;
    let time = |at: &str| {
        chrono::DateTime::parse_from_rfc3339(at)
            .map(|t| t.with_timezone(&chrono::Local))
            .map_err(|_| Error::Denied(format!("job {} has an unreadable time", j.name)))
    };
    let started = time(&j.started_at)?;
    let ended = time(
        j.ended_at
            .as_deref()
            .ok_or_else(|| Error::Denied("job has not ended".into()))?,
    )?;
    let note = serde_json::json!({"job":j.name,"unit":j.unit,"memory_max":j.memory_max,"cpu_quota_percent":j.cpu_quota_percent}).to_string();
    let at = started.to_rfc3339_opts(chrono::SecondsFormat::Millis, false);
    if duration::list(&store, &j.campaign)?
        .iter()
        .any(|r| r["task"] == j.task && r["started_at"] == at && r["note"] == note)
    {
        return Ok(());
    }
    duration::record(
        &store,
        &duration::Span {
            campaign: &j.campaign,
            task: &j.task,
            kind: kind(j),
            started,
            ended,
            outcome: if j.exit_code == Some(0) {
                "ok"
            } else {
                "failed"
            },
            note: &note,
        },
    )
}
pub fn finish(root: &Path, task: &str, name: &str, exit_code: i32) -> Result<Job> {
    let _guard = storage::lock(&root.join("state/.jobs.lock"))?;
    finish_unlocked(root, task, name, exit_code)
}
pub fn stop(root: &Path, task: &str, name: &str) -> Result<()> {
    let j: Job = storage::read(&path(root, task, name)?)?;
    if j.ended_at.is_some() {
        return Ok(());
    }
    let o = io(
        root,
        Command::new("systemctl")
            .args(["--user", "--no-ask-password", "stop", &j.unit])
            .output(),
    )?;
    if !o.status.success() {
        return Err(Error::Denied("owned unit stop failed".into()));
    }
    finish(root, task, name, 143)?;
    Ok(())
}
pub fn constrain_heat(root: &Path, task: &str, name: &str, celsius: f64) -> Result<()> {
    if celsius < 75.0 {
        return Ok(());
    }
    let j: Job = storage::read(&path(root, task, name)?)?;
    if j.ended_at.is_some() {
        return Ok(());
    }
    let o = io(
        root,
        Command::new("systemctl")
            .args([
                "--user",
                "--no-ask-password",
                "set-property",
                "--runtime",
                &j.unit,
                "CPUQuota=25%",
                "CPUWeight=1",
            ])
            .output(),
    )?;
    if !o.status.success() {
        return Err(Error::Denied("thermal job constraint failed".into()));
    }
    Ok(())
}
