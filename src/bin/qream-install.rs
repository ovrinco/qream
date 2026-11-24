//! Thin front over `qream_install`: one JSON line out, exit 0 on success, 1 on refusal, 2 on fault.

use clap::{Parser, Subcommand};
use qream_install::{check, record, Error, Install};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "qream-install",
    about = "Build, check, stage, smoke, switch and roll back QREAM"
)]
struct Cli {
    /// The workspace to install.
    #[arg(long, global = true, default_value = ".")]
    tree: PathBuf,
    /// The releases home.
    #[arg(long, global = true, default_value = "~/.local/lib/qream")]
    home: String,
    #[arg(long, global = true, default_value = "~/.local/bin")]
    bin_dir: String,
    #[arg(long, global = true, default_value = "./target")]
    target_dir: PathBuf,
    #[arg(long, global = true, default_value = "./bin/qream-build-env")]
    build_env: PathBuf,
    #[arg(long, global = true, default_value = "./build.lock")]
    build_lock: PathBuf,
    /// A store file the duration rows go to directly; without it they go through the daemon's
    /// `records.duration`, so the daemon stays the store's one writer.
    #[arg(long, global = true)]
    store: Option<PathBuf>,
    #[arg(long, global = true, default_value = "qream-install")]
    campaign: String,
    #[arg(long, global = true, default_value = "install")]
    task: String,
    #[command(subcommand)]
    verb: Verb,
}

#[derive(Subcommand)]
enum Verb {
    Build,
    Check {
        /// Seconds the check may take: five minutes for the full check, ninety for a targeted one.
        #[arg(long)]
        limit_secs: Option<u64>,
        /// A worker's targeted check: fmt, clippy and the tests of this one crate.
        #[arg(long)]
        only: Option<String>,
    },
    Stage {
        release_id: String,
    },
    Smoke {
        release_id: String,
        /// The live QREAM home, read-only.
        #[arg(long, default_value = "~/.local/share/qream")]
        live_home: String,
        /// Where the private socket lives.
        #[arg(long, default_value = "/tmp")]
        scratch: PathBuf,
    },
    Switch {
        release_id: String,
        #[arg(long)]
        with_tracker: bool,
        #[arg(long, default_value = "./release-archive")]
        archive: PathBuf,
    },
    Rollback,
    Status,
    Adopt {
        #[arg(long, default_value = "./previous")]
        old: PathBuf,
        #[arg(long, default_value = "./legacy")]
        legacy: PathBuf,
    },
}

fn expand(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(path),
    }
}

fn main() {
    // The build identity and the release record come from the package's own front, so the three
    // binaries of a release report one build.
    if matches!(
        std::env::args().nth(1).as_deref(),
        Some("--version" | "release-record")
    ) {
        std::process::exit(qream::cli::run("qream-install"));
    }
    let cli = Cli::parse();
    let code = match run(&cli) {
        Ok(value) => {
            println!("{value}");
            0
        }
        Err(e) => {
            let refused = matches!(
                e.downcast_ref::<Error>(),
                Some(Error::Refused(_) | Error::Step { .. } | Error::Timeout { .. })
            );
            println!(
                "{}",
                serde_json::json!({"ok": false, "error": format!("{e:#}")})
            );
            if refused {
                1
            } else {
                2
            }
        }
    };
    std::process::exit(code);
}

fn run(cli: &Cli) -> anyhow::Result<serde_json::Value> {
    use anyhow::Context;
    let tree =
        std::fs::canonicalize(&cli.tree).with_context(|| format!("tree {}", cli.tree.display()))?;
    let install = Install {
        tree,
        home: expand(&cli.home),
        bin_dir: expand(&cli.bin_dir),
        target_dir: cli.target_dir.clone(),
        build_env: cli.build_env.clone(),
        build_lock: cli.build_lock.clone(),
    };
    let timed = |kind, verb| record::start(kind, verb);
    let save = |t: record::Timed, ok: bool, note: &str| {
        let outcome = if ok { "ok" } else { "refused" };
        match &cli.store {
            Some(store) => record::finish(store, &cli.campaign, &cli.task, t, outcome, note)
                .map(|_| ())
                .context("writing the duration row"),
            None => {
                let ended = qream_install::now();
                qream::cli::call(
                    "records.duration",
                    serde_json::json!({
                        "campaign": cli.campaign,
                        "task": cli.task,
                        "kind": t.kind,
                        "started_at": t.started_at,
                        "ended_at": ended,
                        "outcome": outcome,
                        "note": format!("qream-install {}: {note}", t.verb),
                    }),
                )
                .map(|_| ())
                .context("writing the duration row through the daemon")
            }
        }
    };
    Ok(match &cli.verb {
        Verb::Build => {
            let t = timed("build", "build");
            let r = install.build();
            save(t, r.is_ok(), &install.tree.display().to_string())?;
            serde_json::json!({"ok": true, "seconds": r?})
        }
        Verb::Check { limit_secs, only } => {
            let t = timed("check", "check");
            let (steps, limit) = match only {
                Some(krate) => (
                    install.targeted_check_steps(krate),
                    limit_secs.unwrap_or(check::TARGETED_LIMIT_SECS),
                ),
                None => (
                    install.full_check_steps(),
                    limit_secs.unwrap_or(check::LIMIT_SECS),
                ),
            };
            let r = match only {
                Some(_) => install.check_targeted(steps, limit)?,
                None => install.check_with(steps, limit)?,
            };
            let note = format!("{} tests, {:.1} s", r.tests_passed, r.seconds);
            save(t, r.ok, &note)?;
            if let Some(e) = &r.error {
                return Err(Error::Refused(format!("the full check is red: {e}")).into());
            }
            serde_json::to_value(&r)?
        }
        Verb::Stage { release_id } => {
            let t = timed("install", "stage");
            let r = install.stage(release_id);
            save(t, r.is_ok(), release_id)?;
            serde_json::to_value(r?)?
        }
        Verb::Smoke {
            release_id,
            live_home,
            scratch,
        } => {
            let t = timed("test", "smoke");
            let r = install.smoke(release_id, &expand(live_home), scratch);
            save(t, r.is_ok(), release_id)?;
            serde_json::to_value(r?)?
        }
        Verb::Switch {
            release_id,
            with_tracker,
            archive,
        } => {
            let t = timed("install", "switch");
            let r = install.switch(release_id, *with_tracker, Some(archive));
            save(t, r.is_ok(), release_id)?;
            serde_json::to_value(r?)?
        }
        Verb::Rollback => {
            let t = timed("install", "rollback");
            let r = install.rollback();
            save(t, r.is_ok(), "rollback")?;
            serde_json::to_value(r?)?
        }
        Verb::Status => install.status()?,
        Verb::Adopt { old, legacy } => {
            serde_json::to_value(install.adopt(old, legacy, &install.tree.clone())?)?
        }
    })
}
