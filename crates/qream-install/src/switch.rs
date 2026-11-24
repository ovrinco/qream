//! Switch and roll back. Every link the switch moves is written to `ROLLBACK.tsv` with what it pointed at before;
//! a file in a link's place is moved to `kept/`, never removed. Rollback restores the last switch's rows.

use crate::daemon::{self, UnitRunner, UserUnit};
use crate::error::{At, Error, Result};
use crate::files::{self, Prior};
use crate::smoke::SmokeRecord;
use crate::Install;
use serde::Serialize;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

#[derive(Debug, Serialize)]
pub struct Moved {
    pub link: PathBuf,
    pub previous: String,
    pub new: String,
}

#[derive(Debug, Serialize)]
pub struct SwitchRecord {
    pub seq: u64,
    pub release_id: String,
    pub links: Vec<Moved>,
    pub archived: Option<PathBuf>,
    pub seconds: f64,
}

/// One row of `ROLLBACK.tsv`: `switch|adopt <seq> <at> <release> <link> <previous> <new>` or `rollback <seq> <at>`.
struct Row {
    verb: String,
    seq: u64,
    link: PathBuf,
    previous: String,
}

impl Install {
    fn rows(&self) -> Result<Vec<Row>> {
        let path = self.rollback_file();
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e).at(&path),
        };
        Ok(text
            .lines()
            .filter_map(|line| {
                let f: Vec<&str> = line.split('\t').collect();
                Some(Row {
                    verb: (*f.first()?).to_owned(),
                    seq: f.get(1)?.parse().ok()?,
                    link: PathBuf::from(f.get(4).copied().unwrap_or_default()),
                    previous: f.get(5).copied().unwrap_or_default().to_owned(),
                })
            })
            .collect())
    }

    pub(crate) fn append(&self, lines: &[String]) -> Result<()> {
        std::fs::create_dir_all(&self.home).at(&self.home)?;
        let path = self.rollback_file();
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .at(&path)?;
        for l in lines {
            writeln!(f, "{l}").at(&path)?;
        }
        f.sync_all().at(&path)
    }

    pub(crate) fn next_seq(&self) -> Result<u64> {
        Ok(self.rows()?.iter().map(|r| r.seq).max().unwrap_or(0) + 1)
    }

    /// Move whatever stands at `path` into `kept/`; returns where it went.
    pub(crate) fn keep(&self, path: &Path) -> Result<PathBuf> {
        let kept = self.kept();
        std::fs::create_dir_all(&kept).at(&kept)?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let stamp = chrono::Local::now().format("%Y%m%dT%H%M%S%.f");
        let dest = kept.join(format!("{name}.{stamp}"));
        std::fs::rename(path, &dest).at(path)?;
        Ok(dest)
    }

    /// Point `link` at `target`, returning the `previous` column for it.
    fn repoint(&self, link: &Path, target: &Path) -> Result<String> {
        let previous = match files::prior(link)? {
            Prior::Absent => "absent".to_owned(),
            Prior::Link(t) => format!("link:{}", t.display()),
            Prior::File => format!("kept:{}", self.keep(link)?.display()),
        };
        files::swap_link(link, target)?;
        Ok(previous)
    }

    pub fn switch(
        &self,
        id: &str,
        with_tracker: bool,
        archive: Option<&Path>,
    ) -> Result<SwitchRecord> {
        let mut runner = UserUnit::from_env()?;
        self.switch_with_runner(id, with_tracker, archive, &mut runner)
    }

    /// The production switch with an injected user-unit boundary for isolated tests.
    pub fn switch_with_runner(
        &self,
        id: &str,
        with_tracker: bool,
        archive: Option<&Path>,
        runner: &mut impl UnitRunner,
    ) -> Result<SwitchRecord> {
        crate::valid_id(id)?;
        let start = Instant::now();
        let dir = self.release(id);
        let smoke_path = dir.join("SMOKE.json");
        let smoke: SmokeRecord =
            serde_json::from_slice(&std::fs::read(&smoke_path).map_err(|_| {
                Error::Refused(format!(
                    "release {id} has no smoke check; run `qream-install smoke`"
                ))
            })?)?;
        let sums = std::fs::read_to_string(dir.join("SHA256SUMS")).at(&dir)?;
        let sums_now = files::sums(&dir)?;
        let digest = files::sha256_text(&sums);
        if !smoke.ok || smoke.sums_sha256 != digest || sums != sums_now {
            return Err(Error::Refused(format!(
                "release {id}: the smoke check is {} or the release changed since it",
                if smoke.ok { "green" } else { "red" }
            )));
        }
        let seq = self.next_seq()?;
        let mut links = vec![(self.current(), dir.clone())];
        let mut names = vec!["qream", "qream-install"];
        if with_tracker {
            if !dir.join("bin/tracker").is_file() {
                return Err(Error::Refused(format!("release {id} carries no tracker")));
            }
            names.push("tracker");
        }
        for name in names {
            links.push((
                self.bin_dir.join(name),
                self.current().join("bin").join(name),
            ));
        }
        let mut moved = Vec::new();
        let mut lines = Vec::new();
        for (link, target) in links {
            let previous = self.repoint(&link, &target)?;
            lines.push(format!(
                "switch\t{seq}\t{}\t{id}\t{}\t{previous}\t{}",
                crate::now(),
                link.display(),
                target.display()
            ));
            moved.push(Moved {
                link,
                previous,
                new: target.display().to_string(),
            });
        }
        self.append(&lines)?;
        let deadline = Instant::now() + daemon::SWITCH_WAIT;
        let result = runner
            .restart(&self.current().join("bin/qreamd"), deadline)
            .and_then(|()| daemon::converge(self, runner, id, deadline));
        if let Err(error) = result {
            self.rollback_links()?;
            let restored = runner.restart(
                &self.current().join("bin/qreamd"),
                Instant::now() + daemon::SWITCH_WAIT,
            );
            return Err(Error::Refused(format!(
                "{error}; rolled back the release and command links; previous daemon restart: {}",
                match restored {
                    Ok(()) => "ok".into(),
                    Err(e) => e.to_string(),
                }
            )));
        }
        let archived = match archive {
            Some(root) => {
                let dest = root.join(id);
                if !dest.exists() {
                    files::copy_dir(&dir, &dest)?;
                }
                Some(dest)
            }
            None => None,
        };
        Ok(SwitchRecord {
            seq,
            release_id: id.into(),
            links: moved,
            archived,
            seconds: start.elapsed().as_secs_f64(),
        })
    }

    /// Undo the last switch (or adopt) not yet rolled back, newest link first.
    pub fn rollback(&self) -> Result<SwitchRecord> {
        let mut runner = UserUnit::from_env()?;
        self.rollback_with_runner(&mut runner)
    }

    pub fn rollback_with_runner(&self, runner: &mut impl UnitRunner) -> Result<SwitchRecord> {
        let record = self.rollback_links()?;
        let deadline = Instant::now() + daemon::SWITCH_WAIT;
        runner.restart(&self.current().join("bin/qreamd"), deadline)?;
        if let Some(id) = std::fs::read_link(self.current())
            .ok()
            .and_then(|p| p.file_name().map(|s| s.to_string_lossy().into_owned()))
        {
            daemon::converge(self, runner, &id, deadline)?;
        }
        Ok(record)
    }

    fn rollback_links(&self) -> Result<SwitchRecord> {
        let start = Instant::now();
        let rows = self.rows()?;
        let done: Vec<u64> = rows
            .iter()
            .filter(|r| r.verb == "rollback")
            .map(|r| r.seq)
            .collect();
        let seq = rows
            .iter()
            .filter(|r| r.verb == "switch" && !done.contains(&r.seq))
            .map(|r| r.seq)
            .max()
            .ok_or_else(|| Error::Refused("no switch to roll back".into()))?;
        let mut moved = Vec::new();
        for row in rows
            .iter()
            .rev()
            .filter(|r| r.seq == seq && r.verb == "switch")
        {
            let link = &row.link;
            let now = match files::prior(link)? {
                Prior::Link(t) => t.display().to_string(),
                _ => String::new(),
            };
            if let Some(target) = row.previous.strip_prefix("link:") {
                files::swap_link(link, Path::new(target))?;
            } else if let Some(file) = row.previous.strip_prefix("kept:") {
                if !matches!(files::prior(link)?, Prior::Absent) {
                    self.keep(link)?;
                }
                std::fs::rename(file, link).at(Path::new(file))?;
            } else if !matches!(files::prior(link)?, Prior::Absent) {
                self.keep(link)?;
            }
            moved.push(Moved {
                link: link.clone(),
                previous: now,
                new: row.previous.clone(),
            });
        }
        self.append(&[format!("rollback\t{seq}\t{}", crate::now())])?;
        Ok(SwitchRecord {
            seq,
            release_id: String::new(),
            links: moved,
            archived: None,
            seconds: start.elapsed().as_secs_f64(),
        })
    }

    pub fn status(&self) -> Result<serde_json::Value> {
        let current = std::fs::read_link(self.current()).ok();
        let mut releases = Vec::new();
        if let Ok(read) = std::fs::read_dir(self.releases()) {
            for e in read.flatten() {
                releases.push(e.file_name().to_string_lossy().into_owned());
            }
        }
        releases.sort();
        let check: Option<serde_json::Value> = std::fs::read(self.check_file())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok());
        let rollback = std::fs::read_to_string(self.rollback_file()).unwrap_or_default();
        let tail: Vec<&str> = rollback.lines().rev().take(10).collect();
        Ok(serde_json::json!({
            "home": self.home,
            "current": current,
            "releases": releases,
            "check": check.map(|c| serde_json::json!({
                "ok": c.get("ok"), "seconds": c.get("seconds"),
                "tests_passed": c.get("tests_passed"), "at": c.get("at")
            })),
            "rollback_tail": tail,
        }))
    }
}
