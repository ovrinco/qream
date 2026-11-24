//! User-unit restart and bounded build observation. The switch accepts a fake runner in tests.
use crate::{error::At, run, Error, Install, Result};
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

pub const SWITCH_WAIT: Duration = Duration::from_secs(30);

pub trait UnitRunner {
    fn restart(&mut self, daemon: &Path, deadline: Instant) -> Result<()>;
    fn build(&mut self, deadline: Instant) -> Result<String>;
}

pub struct UserUnit {
    unit: String,
    socket: PathBuf,
    environment: Vec<(String, String)>,
}
impl UserUnit {
    pub fn from_env() -> Result<Self> {
        let unit = std::env::var("QREAM_UNIT").unwrap_or_else(|_| "qream-daemon".into());
        let socket = match std::env::var_os("QREAM_SOCKET") {
            Some(p) => p.into(),
            None => PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").ok_or_else(|| {
                Error::Refused("the user runtime directory is unavailable".into())
            })?)
            .join("qream/engine.sock"),
        };
        let environment = [
            "QREAM_HOME",
            "QREAM_SOCKET",
            "QREAM_STORE",
            "QREAM_UNIT",
            "QREAM_READ_ONLY",
            "QREAM_PRIVATE_DIR",
            "QREAM_CODEX_SESSIONS",
            "QREAM_CLAUDE_PROJECTS",
            "QREAM_LAUNCH_SETTLE_MS",
        ]
        .into_iter()
        .filter_map(|key| std::env::var(key).ok().map(|value| (key.to_owned(), value)))
        .collect();
        Ok(Self {
            unit,
            socket,
            environment,
        })
    }
}
impl UserUnit {
    fn restart_with(
        &mut self,
        daemon: &Path,
        deadline: Instant,
        mut execute: impl FnMut(&str, &[String], &Path, Instant) -> Result<std::process::Output>,
    ) -> Result<()> {
        let cwd = daemon
            .parent()
            .ok_or_else(|| Error::Refused("daemon path has no parent".into()))?;
        // The on-demand unit is transient and its ExecStart points to a concrete old release.
        // Recreate that same unit with the newly selected executable; restarting its old ExecStart
        // would run the previous release again. No terminal/session commands are issued here.
        let show = vec![
            "systemctl".into(),
            "--user".into(),
            "show".into(),
            self.unit.clone(),
            "-p".into(),
            "LoadState".into(),
            "-p".into(),
            "ActiveState".into(),
        ];
        let state = execute("read daemon user unit state", &show, cwd, deadline)?;
        let text = String::from_utf8_lossy(&state.stdout);
        let property = |name: &str| {
            text.lines()
                .find_map(|line| line.strip_prefix(name))
                .filter(|value| !value.is_empty())
                .ok_or_else(|| Error::Refused(format!("daemon unit state lacks {name}")))
        };
        let loaded = property("LoadState=")? == "loaded";
        let active = property("ActiveState=")?;
        // An intentionally stopped transient unit may have been collected already.
        // Switch and rollback share this restart path; neither needs to stop it again.
        if loaded && !matches!(active, "inactive" | "failed") {
            let stop = vec![
                "systemctl".into(),
                "--user".into(),
                "stop".into(),
                self.unit.clone(),
            ];
            execute("stop daemon user unit", &stop, cwd, deadline)?;
        }
        let mut start = vec![
            "systemd-run".into(),
            "--user".into(),
            "--collect".into(),
            "--quiet".into(),
            "--unit".into(),
            self.unit.clone(),
        ];
        for (key, value) in &self.environment {
            start.push(format!("--setenv={key}={value}"));
        }
        start.push(daemon.to_string_lossy().into_owned());
        execute("start daemon user unit", &start, cwd, deadline)?;
        Ok(())
    }
}
impl UnitRunner for UserUnit {
    fn restart(&mut self, daemon: &Path, deadline: Instant) -> Result<()> {
        self.restart_with(daemon, deadline, |step, argv, cwd, deadline| {
            run::one(step, argv, cwd, deadline, 30)
        })
    }
    fn build(&mut self, deadline: Instant) -> Result<String> {
        let mut socket = UnixStream::connect(&self.socket).at(&self.socket)?;
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(Error::Timeout {
                step: "daemon build".into(),
                limit_secs: 30,
            });
        }
        // Each observation gets at most one second, within the total thirty-second deadline.
        let timeout = Some(left.min(Duration::from_secs(1)));
        socket.set_read_timeout(timeout).at(&self.socket)?;
        socket.set_write_timeout(timeout).at(&self.socket)?;
        let mut reader = BufReader::new(socket.try_clone().at(&self.socket)?);
        let request = json!({"v":1,"id":"install-ping","cmd":"ping","args":{}}).to_string() + "\n";
        socket.write_all(request.as_bytes()).at(&self.socket)?;
        loop {
            if Instant::now() >= deadline {
                return Err(Error::Timeout {
                    step: "daemon ping".into(),
                    limit_secs: 30,
                });
            }
            let mut line = String::new();
            if reader.read_line(&mut line).at(&self.socket)? == 0 {
                return Err(Error::Refused("daemon closed its version route".into()));
            }
            let value: Value = serde_json::from_str(&line)?;
            if value["id"] == "install-ping" {
                if value["ok"] != true {
                    return Err(Error::Refused(format!(
                        "daemon ping refused: {}",
                        value["error"]
                    )));
                }
                return value["result"]["build"]
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| Error::Refused("daemon ping lacks a build".into()));
            }
        }
    }
}

pub(crate) fn converge(
    install: &Install,
    runner: &mut impl UnitRunner,
    id: &str,
    deadline: Instant,
) -> Result<()> {
    let root = install.release(id);
    let path = root.join("release_record.json");
    let record: Value = serde_json::from_slice(&std::fs::read(&path).at(&path)?)?;
    let expected = record
        .get("build")
        .ok_or_else(|| Error::Refused("release record lacks build identity".into()))?;
    let argv = vec![
        install
            .current()
            .join("bin/qream")
            .to_string_lossy()
            .into_owned(),
        "--version".into(),
    ];
    let output = run::one("selected qream --version", &argv, &root, deadline, 30)?;
    let cli: Value = serde_json::from_slice(&output.stdout)?;
    if cli != *expected {
        return Err(Error::Refused(format!(
            "qream --version reports {cli}, release record reports {expected}"
        )));
    }
    let mut last = String::new();
    while Instant::now() < deadline {
        match runner.build(deadline) {
            Ok(build) if build == id => return Ok(()),
            Ok(build) => last = format!("daemon reports {build}, selected release is {id}"),
            Err(e) => last = e.to_string(),
        }
        std::thread::sleep(
            Duration::from_millis(50).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
    Err(Error::Refused(format!(
        "builds did not agree within thirty seconds: {last}"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;
    use std::process::{ExitStatus, Output};

    fn unit() -> UserUnit {
        UserUnit {
            unit: "private-installer-test".into(),
            socket: PathBuf::from("unused"),
            environment: vec![],
        }
    }
    fn output(stdout: &str) -> Output {
        Output {
            status: ExitStatus::from_raw(0),
            stdout: stdout.as_bytes().to_vec(),
            stderr: vec![],
        }
    }
    #[test]
    fn restart_absent_inactive_or_failed_unit_starts_new_daemon() {
        for state in [
            "LoadState=not-found\nActiveState=inactive\n",
            "LoadState=loaded\nActiveState=inactive\n",
            "LoadState=loaded\nActiveState=failed\n",
        ] {
            let daemon = Path::new("/private/release/bin/qreamd");
            let mut commands = Vec::new();
            unit()
                .restart_with(daemon, Instant::now() + SWITCH_WAIT, |_, argv, _, _| {
                    commands.push(argv.to_vec());
                    Ok(output(if argv[0] == "systemctl" { state } else { "" }))
                })
                .expect("already stopped unit restarts");
            assert_eq!(commands.len(), 2);
            assert_eq!(
                commands[0],
                [
                    "systemctl",
                    "--user",
                    "show",
                    "private-installer-test",
                    "-p",
                    "LoadState",
                    "-p",
                    "ActiveState"
                ]
            );
            assert_eq!(commands[1][0], "systemd-run");
            assert_eq!(
                commands[1].last().expect("daemon argument"),
                &daemon.to_string_lossy()
            );
        }
    }
    #[test]
    fn restart_loaded_active_unit_refuses_real_stop_failure() {
        let mut commands = Vec::new();
        let error = unit()
            .restart_with(
                Path::new("/private/release/bin/qreamd"),
                Instant::now() + SWITCH_WAIT,
                |step, argv, _, _| {
                    commands.push(argv.to_vec());
                    if argv[2] == "show" {
                        Ok(output("LoadState=loaded\nActiveState=active\n"))
                    } else {
                        Err(Error::Step {
                            step: step.into(),
                            detail: "stop denied".into(),
                        })
                    }
                },
            )
            .expect_err("active unit stop failure must refuse");
        assert!(error.to_string().contains("stop denied"));
        assert_eq!(commands.len(), 2);
        assert_eq!(
            commands[1],
            ["systemctl", "--user", "stop", "private-installer-test"]
        );
        assert!(!commands.iter().any(|argv| argv[0] == "systemd-run"));
    }
}
