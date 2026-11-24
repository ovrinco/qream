//! What QREAM started that no running task or open campaign needs, and ending exactly that.
//! Only the registry's records are considered: a process QREAM did not start is never listed.
use crate::{
    Result,
    control::{HoldKind, Scope, UnitStatus, load_holds, load_units},
    owned::{self, Ended, Handle, Liveness, Owned, Registry, Role},
    storage::read_json,
};
use serde::{Deserialize, Serialize};
use std::{path::Path, time::Duration};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Leftover {
    pub id: String,
    pub campaign: String,
    pub task: Option<String>,
    pub role: Role,
    pub purpose: String,
    /// The process group or the unit name.
    pub what: String,
    pub pids: Vec<u32>,
    pub why: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EndedLeftover {
    #[serde(flatten)]
    pub leftover: Leftover,
    pub outcome: String,
}

#[derive(Default, Deserialize)]
struct Close {
    actual_close: Option<String>,
}

/// Why nothing needs this record any more, or None while something does.
fn unneeded(o: &Owned) -> Result<Option<String>> {
    let close: Close = read_json(&o.root.join("CLOSE.json"))?;
    if close.actual_close.is_some() {
        return Ok(Some("its campaign has closed".into()));
    }
    let holds = load_holds(&o.root)?;
    if holds
        .iter()
        .any(|h| h.scope == Scope::Campaign && (h.kind == HoldKind::Kill || !h.pending))
    {
        return Ok(Some("its campaign is stopped".into()));
    }
    let units = load_units(&o.root)?;
    let running = |s: UnitStatus| matches!(s, UnitStatus::Live | UnitStatus::Launching);
    match &o.task {
        Some(task) => Ok(match units.get(task) {
            Some(u) if running(u.status) => None,
            Some(_) => Some(format!("task {task} is stopped")),
            None => Some(format!("task {task} is not running")),
        }),
        None if units.values().any(|u| running(u.status)) => Ok(None),
        None => Ok(Some("no task of its campaign runs".into())),
    }
}

/// Lists the owned records that still run and are not needed; records already gone or whose id now
/// belongs to someone else are dropped from the registry without any signal.
pub fn list(registry: &Registry, campaign: Option<&str>) -> Result<Vec<Leftover>> {
    let mut found = Vec::new();
    for o in registry.list()? {
        if campaign.is_some_and(|c| c != o.campaign) {
            continue;
        }
        let pids = match owned::liveness(&o)? {
            Liveness::Alive(pids) => pids,
            Liveness::Gone | Liveness::Foreign(_) => {
                registry.remove(&o.id)?;
                continue;
            }
        };
        if let Some(why) = unneeded(&o)? {
            found.push(Leftover {
                what: match &o.handle {
                    Handle::Group { pgid, .. } => format!("process group {pgid}"),
                    Handle::Unit { name, .. } => format!("unit {name}"),
                },
                id: o.id,
                campaign: o.campaign,
                task: o.task,
                role: o.role,
                purpose: o.purpose,
                pids,
                why,
            });
        }
    }
    Ok(found)
}

/// Ends exactly the listed leftovers.
pub fn kill(
    registry: &Registry,
    campaign: Option<&str>,
    grace: Duration,
) -> Result<Vec<EndedLeftover>> {
    let records = registry.list()?;
    let mut ended = Vec::new();
    for leftover in list(registry, campaign)? {
        let Some(o) = records.iter().find(|o| o.id == leftover.id) else {
            continue;
        };
        let outcome = match owned::end(o, grace) {
            Ok(Ended::Ended) => "ended".to_string(),
            Ok(Ended::AlreadyGone) => "already gone".to_string(),
            Err(crate::Error::NotOwned(why)) => format!("not signalled: {why}"),
            Err(e) => return Err(e),
        };
        registry.remove(&o.id)?;
        ended.push(EndedLeftover { leftover, outcome });
    }
    Ok(ended)
}

/// Whether a campaign root is open for leftovers' purposes (used by the command line's summary).
pub fn campaign_open(root: &Path) -> Result<bool> {
    Ok(read_json::<Close>(&root.join("CLOSE.json"))?
        .actual_close
        .is_none())
}
