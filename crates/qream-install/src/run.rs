//! Child processes under a deadline: each in its own process group, killed whole when the clock runs out.

use crate::error::{At, Error, Result};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

/// The lowest-priority prefix of the build line: cores 3 to 6, nice 19, idle I/O.
pub const LOW: &[&str] = &["taskset", "-c", "3-6", "nice", "-n19", "ionice", "-c3"];

pub fn spawn(argv: &[String], cwd: &Path) -> Result<Child> {
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| Error::Refused("empty command".into()))?;
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    command.spawn().at(cwd)
}

fn kill_group(child: &mut Child) {
    let group = format!("-{}", child.id());
    // The group goes first so cargo and its test binaries die with the shell that started them.
    let _ = Command::new("kill")
        .args(["-KILL", "--", &group])
        .stderr(Stdio::null())
        .status();
    let _ = child.kill();
}

/// Wait for all children together; past `deadline` every one still running is killed and the step times out.
pub fn wait_all(
    step: &str,
    children: Vec<Child>,
    deadline: Instant,
    limit_secs: u64,
) -> Result<Vec<Output>> {
    let mut readers = Vec::new();
    let mut running = Vec::new();
    for mut child in children {
        let out = child.stdout.take();
        let err = child.stderr.take();
        readers.push(std::thread::spawn(move || {
            use std::io::Read;
            let mut o = Vec::new();
            let mut e = Vec::new();
            let stdout = std::thread::spawn(move || {
                if let Some(mut s) = out {
                    let _ = s.read_to_end(&mut o);
                }
                o
            });
            if let Some(mut s) = err {
                let _ = s.read_to_end(&mut e);
            }
            (stdout.join().unwrap_or_default(), e)
        }));
        running.push(Some(child));
    }
    let mut statuses = vec![None; running.len()];
    loop {
        let mut left = 0;
        for (slot, status) in running.iter_mut().zip(statuses.iter_mut()) {
            if let Some(child) = slot {
                match child.try_wait() {
                    Ok(Some(done)) => {
                        *status = Some(done);
                        *slot = None;
                    }
                    Ok(None) => left += 1,
                    Err(e) => {
                        return Err(Error::Step {
                            step: step.into(),
                            detail: e.to_string(),
                        })
                    }
                }
            }
        }
        if left == 0 {
            break;
        }
        if Instant::now() >= deadline {
            for child in running.iter_mut().flatten() {
                kill_group(child);
                let _ = child.wait();
            }
            return Err(Error::Timeout {
                step: step.into(),
                limit_secs,
            });
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let mut outputs = Vec::new();
    for (reader, status) in readers.into_iter().zip(statuses) {
        let (stdout, stderr) = reader.join().unwrap_or_default();
        if let Some(status) = status {
            outputs.push(Output {
                status,
                stdout,
                stderr,
            });
        }
    }
    Ok(outputs)
}

/// One command under the deadline; a non-zero exit is the step's failure with the tail of its output.
pub fn one(
    step: &str,
    argv: &[String],
    cwd: &Path,
    deadline: Instant,
    limit: u64,
) -> Result<Output> {
    let child = spawn(argv, cwd)?;
    let mut outputs = wait_all(step, vec![child], deadline, limit)?;
    let output = outputs.pop().ok_or_else(|| Error::Step {
        step: step.into(),
        detail: "no exit status".into(),
    })?;
    if !output.status.success() {
        return Err(Error::Step {
            step: step.into(),
            detail: tail(&output),
        });
    }
    Ok(output)
}

pub fn tail(output: &Output) -> String {
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(20)..].join("\n")
}
