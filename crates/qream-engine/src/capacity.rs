//! Resource-group ceilings. The minimum binds every admitted start.
use crate::{Error, Result, storage::io};
use serde::{Deserialize, Serialize};
use std::{fs, path::Path, process::Command};
pub const MEMORY_RESERVE: u64 = 16 * 1024 * 1024 * 1024;
/// Waiting agents per swarm core: an agent mostly waits on its provider, and heavy work (builds,
/// tests) is held to cores 3-6 at the lowest priority by the build lock and the load guard, so one
/// core carries several agents (row RW-16-R06: slots for heavy work, not for idle waits).
pub const AGENTS_PER_CORE: u64 = 3;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Resources {
    pub available_memory: u64,
    pub cpu_threads: u64,
    pub disk_available: u64,
    pub active_sessions: u64,
    pub provider_slots: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Limits {
    pub memory_per_task: u64,
    pub threads_per_task: u64,
    pub disk_reserve: u64,
    pub disk_per_task: u64,
    pub sessions: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ceiling {
    pub resource: String,
    pub slots: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capacity {
    pub ceilings: Vec<Ceiling>,
    pub binding: String,
    pub slots: u64,
    pub memory_reserve: u64,
    pub memory_per_task: u64,
    pub threads_per_task: u64,
}
pub fn calculate(r: &Resources, l: &Limits) -> Result<Capacity> {
    if l.memory_per_task == 0 || l.threads_per_task == 0 || l.disk_per_task == 0 {
        return Err(Error::Denied("resource costs must be positive".into()));
    }
    let ceilings = vec![
        Ceiling {
            resource: "memory".into(),
            slots: r.available_memory.saturating_sub(MEMORY_RESERVE) / l.memory_per_task,
        },
        Ceiling {
            resource: "cpu_threads".into(),
            slots: r.cpu_threads * AGENTS_PER_CORE / l.threads_per_task,
        },
        Ceiling {
            resource: "disk".into(),
            slots: r.disk_available.saturating_sub(l.disk_reserve) / l.disk_per_task,
        },
        Ceiling {
            resource: "sessions".into(),
            slots: l.sessions.saturating_sub(r.active_sessions),
        },
        Ceiling {
            resource: "provider_windows".into(),
            slots: r.provider_slots,
        },
    ];
    let binding = ceilings
        .iter()
        .min_by_key(|c| c.slots)
        .ok_or_else(|| Error::Denied("no ceilings".into()))?;
    Ok(Capacity {
        binding: binding.resource.clone(),
        slots: binding.slots,
        ceilings,
        memory_reserve: MEMORY_RESERVE,
        memory_per_task: l.memory_per_task,
        threads_per_task: l.threads_per_task,
    })
}
pub fn machine(path: &Path, active_sessions: u64, provider_slots: u64) -> Result<Resources> {
    let p = Path::new("/proc/meminfo");
    let mem = io(p, fs::read_to_string(p))?;
    let available_memory = mem
        .lines()
        .find_map(|l| {
            l.strip_prefix("MemAvailable:")
                .and_then(|s| s.split_whitespace().next())
                .and_then(|s| s.parse::<u64>().ok())
        })
        .ok_or_else(|| Error::Denied("MemAvailable missing".into()))?
        * 1024;
    let p = Path::new("/proc/self/status");
    let status = io(p, fs::read_to_string(p))?;
    let cpus = status
        .lines()
        .find_map(|s| s.strip_prefix("Cpus_allowed_list:\t"))
        .ok_or_else(|| Error::Denied("CPU affinity unavailable".into()))?;
    let mut cpu_threads = 0;
    for range in cpus.split(',') {
        let (a, b) = range.split_once('-').unwrap_or((range, range));
        let a: u64 = a
            .parse()
            .map_err(|_| Error::Denied("invalid CPU list".into()))?;
        let b: u64 = b
            .parse()
            .map_err(|_| Error::Denied("invalid CPU range".into()))?;
        cpu_threads += (a..=b).filter(|cpu| (3..=6).contains(cpu)).count() as u64;
    }
    let output = io(
        path,
        Command::new("df")
            .args(["-B1", "--output=avail"])
            .arg(path)
            .output(),
    )?;
    if !output.status.success() {
        return Err(Error::Denied("disk availability failed".into()));
    }
    let disk_available = String::from_utf8_lossy(&output.stdout)
        .lines()
        .nth(1)
        .and_then(|s| s.trim().parse().ok())
        .ok_or_else(|| Error::Denied("disk availability missing".into()))?;
    Ok(Resources {
        available_memory,
        cpu_threads,
        disk_available,
        active_sessions,
        provider_slots,
    })
}
impl Capacity {
    pub fn admit(&self) -> Result<()> {
        if self.slots == 0 {
            Err(Error::Denied(format!("capacity held by {}", self.binding)))
        } else {
            Ok(())
        }
    }
}
/// Thermal readings are supplied by the daemon's hardware monitor. Unknown heat fails closed.
pub fn thermal_admit(celsius: Option<f64>) -> Result<()> {
    if celsius.is_some_and(|t| t.is_finite() && t < 75.0) {
        Ok(())
    } else {
        Err(Error::Denied("waiting on heat line".into()))
    }
}
