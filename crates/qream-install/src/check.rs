//! The build line and the full check: fmt, clippy `-D warnings`, the tests run concurrently, qmllint on `ui/`.
//! One wall clock binds the whole check; past it every child is killed and the check is red.

use crate::error::{At, Error, Result};
use crate::{files, run, Install};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Instant;

/// The full check's wall clock (RW-16-R03).
pub const LIMIT_SECS: u64 = 300;
/// A worker's targeted check's wall clock (RW-16-R04).
pub const TARGETED_LIMIT_SECS: u64 = 90;

pub enum Step {
    /// A command that must exit 0.
    Cmd { name: String, argv: Vec<String> },
    /// `cargo test --no-run --message-format=json`: every test binary it names then runs at once.
    Tests { argv: Vec<String> },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StepTime {
    pub name: String,
    pub seconds: f64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CheckRecord {
    pub tree: PathBuf,
    pub tree_sha256: String,
    pub ok: bool,
    pub seconds: f64,
    pub limit_secs: u64,
    pub tests_passed: u64,
    pub test_binaries: usize,
    pub steps: Vec<StepTime>,
    pub error: Option<String>,
    pub at: String,
}

impl Install {
    /// `cargo +1.94.0 <args>` on the build line: build env, shared target, build lock, cores 3-6 at nice 19.
    pub fn cargo(&self, args: &[&str]) -> Vec<String> {
        let script = format!(
            "source \"$1\"; export CARGO_TARGET_DIR=\"$2\"; lock=\"$3\"; shift 3; exec flock \"$lock\" {} cargo +1.94.0 \"$@\"",
            run::LOW.join(" ")
        );
        let mut argv: Vec<String> = vec!["bash".into(), "-c".into(), script, "bash".into()];
        for p in [&self.build_env, &self.target_dir, &self.build_lock] {
            argv.push(p.to_string_lossy().into_owned());
        }
        argv.extend(args.iter().map(|a| (*a).to_owned()));
        argv
    }

    pub fn build(&self) -> Result<f64> {
        let start = Instant::now();
        let argv = self.cargo(&["build", "--workspace", "--offline", "--locked", "-j4"]);
        // The build line's own budget is twelve minutes.
        run::one(
            "build",
            &argv,
            &self.tree,
            start + std::time::Duration::from_secs(720),
            720,
        )?;
        Ok(start.elapsed().as_secs_f64())
    }

    pub fn full_check_steps(&self) -> Vec<Step> {
        let mut steps = vec![
            Step::Cmd {
                name: "fmt".into(),
                argv: self.cargo(&["fmt", "--all", "--", "--check"]),
            },
            Step::Cmd {
                name: "clippy".into(),
                argv: self.cargo(&[
                    "clippy",
                    "--workspace",
                    "--all-targets",
                    "--offline",
                    "--locked",
                    "-j4",
                    "--",
                    "-D",
                    "warnings",
                ]),
            },
            Step::Tests {
                argv: self.cargo(&[
                    "test",
                    "--workspace",
                    "--no-run",
                    "--offline",
                    "--locked",
                    "-j4",
                    "--message-format=json",
                ]),
            },
        ];
        let words = self.tree.join("tools/words_check.sh");
        if words.is_file() {
            steps.push(Step::Cmd {
                name: "words".into(),
                argv: vec!["sh".into(), words.to_string_lossy().into_owned()],
            });
        }
        let ui = self.tree.join("ui");
        if ui.is_dir() {
            // Qt 6's qmllint; `/usr/bin/qmllint` is the old tool that exits 255 in silence.
            let qt6 = Path::new("/usr/lib/qt6/bin/qmllint");
            let lint = if qt6.is_file() {
                qt6.to_string_lossy().into_owned()
            } else {
                "qmllint".to_owned()
            };
            let mut argv = vec![lint];
            if let Ok(found) = files::walk(&ui) {
                argv.extend(
                    found
                        .iter()
                        .filter(|p| p.extension().is_some_and(|e| e == "qml"))
                        .map(|p| ui.join(p).to_string_lossy().into_owned()),
                );
            }
            if argv.len() > 1 {
                steps.push(Step::Cmd {
                    name: "qmllint".into(),
                    argv,
                });
            }
        }
        steps
    }

    /// A worker's targeted check: only what its crate's change can affect (fmt, clippy, its tests).
    pub fn targeted_check_steps(&self, krate: &str) -> Vec<Step> {
        vec![
            Step::Cmd {
                name: format!("fmt {krate}"),
                argv: self.cargo(&["fmt", "-p", krate, "--", "--check"]),
            },
            Step::Cmd {
                name: format!("clippy {krate}"),
                argv: self.cargo(&[
                    "clippy",
                    "-p",
                    krate,
                    "--all-targets",
                    "--offline",
                    "--locked",
                    "-j4",
                    "--",
                    "-D",
                    "warnings",
                ]),
            },
            Step::Tests {
                argv: self.cargo(&[
                    "test",
                    "-p",
                    krate,
                    "--no-run",
                    "--offline",
                    "--locked",
                    "-j4",
                    "--message-format=json",
                ]),
            },
        ]
    }

    /// A targeted check: same runner, its record in `TARGETED.json`; never the install's `CHECK.json`.
    pub fn check_targeted(&self, steps: Vec<Step>, limit_secs: u64) -> Result<CheckRecord> {
        self.check_into(steps, limit_secs, &self.home.join("TARGETED.json"))
    }

    /// Run `steps` under one wall clock of `limit_secs`; the record lands in `CHECK.json` green or red.
    pub fn check_with(&self, steps: Vec<Step>, limit_secs: u64) -> Result<CheckRecord> {
        self.check_into(steps, limit_secs, &self.check_file())
    }

    fn check_into(&self, steps: Vec<Step>, limit_secs: u64, path: &Path) -> Result<CheckRecord> {
        let start = Instant::now();
        let deadline = start + std::time::Duration::from_secs(limit_secs);
        let tree_sha256 = files::tree_hash(&self.tree)?;
        let mut times = Vec::new();
        let mut passed = 0;
        let mut binaries = 0;
        let mut error = None;
        for step in steps {
            let begun = Instant::now();
            let (name, result) = match step {
                Step::Cmd { name, argv } => {
                    let r = run::one(&name, &argv, &self.tree, deadline, limit_secs).map(|_| ());
                    (name, r)
                }
                Step::Tests { argv } => {
                    let r = tests(&self.tree, &argv, deadline, limit_secs).map(|(p, b)| {
                        passed = p;
                        binaries = b;
                    });
                    ("tests".to_owned(), r)
                }
            };
            times.push(StepTime {
                name,
                seconds: begun.elapsed().as_secs_f64(),
            });
            if let Err(e) = result {
                error = Some(e.to_string());
                break;
            }
        }
        let record = CheckRecord {
            tree: self.tree.clone(),
            tree_sha256,
            ok: error.is_none(),
            seconds: start.elapsed().as_secs_f64(),
            limit_secs,
            tests_passed: passed,
            test_binaries: binaries,
            steps: times,
            error,
            at: crate::now(),
        };
        std::fs::create_dir_all(&self.home).at(&self.home)?;
        std::fs::write(path, serde_json::to_vec_pretty(&record)?).at(path)?;
        Ok(record)
    }
}

#[derive(Deserialize)]
struct Artifact {
    reason: String,
    executable: Option<PathBuf>,
    manifest_path: Option<PathBuf>,
    profile: Option<Profile>,
}

#[derive(Deserialize)]
struct Profile {
    test: bool,
}

/// Build the test binaries, then run every one at once at the lowest priority; returns (tests passed, binaries).
fn tests(tree: &Path, argv: &[String], deadline: Instant, limit: u64) -> Result<(u64, usize)> {
    let built = run::one("tests (build)", argv, tree, deadline, limit)?;
    let mut targets = Vec::new();
    for line in String::from_utf8_lossy(&built.stdout).lines() {
        let Ok(a) = serde_json::from_str::<Artifact>(line) else {
            continue;
        };
        if a.reason != "compiler-artifact" || !a.profile.is_some_and(|p| p.test) {
            continue;
        }
        if let (Some(exe), Some(manifest)) = (a.executable, a.manifest_path) {
            let dir = manifest.parent().unwrap_or(tree).to_owned();
            targets.push((exe, dir));
        }
    }
    let mut children = Vec::new();
    for (exe, dir) in &targets {
        let mut cmd: Vec<String> = run::LOW.iter().map(|s| (*s).to_owned()).collect();
        cmd.push(exe.to_string_lossy().into_owned());
        children.push(run::spawn(&cmd, dir)?);
    }
    let outputs = run::wait_all("tests", children, deadline, limit)?;
    let mut passed = 0;
    for (output, (exe, _)) in outputs.iter().zip(&targets) {
        if !output.status.success() {
            return Err(Error::Step {
                step: format!("tests {}", exe.display()),
                detail: run::tail(output),
            });
        }
        passed += count_passed(&String::from_utf8_lossy(&output.stdout));
    }
    Ok((passed, targets.len()))
}

/// The sum of `N passed` over every `test result:` line.
pub fn count_passed(text: &str) -> u64 {
    text.lines()
        .filter_map(|l| l.strip_prefix("test result: "))
        .filter_map(|l| l.split(". ").nth(1))
        .filter_map(|l| l.split(' ').next())
        .filter_map(|n| n.parse::<u64>().ok())
        .sum()
}

#[cfg(test)]
mod unit {
    #[test]
    fn counts_passed_tests_over_binaries() {
        let text = "running 2 tests\ntest result: ok. 2 passed; 0 failed; 0 ignored\n\
                    test result: ok. 11 passed; 0 failed; 1 ignored; 0 measured\n";
        assert_eq!(super::count_passed(text), 13);
    }
}
