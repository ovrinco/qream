//! The campaign verbs of CONTRACTS.md §6 that run on the daemon's controller and its
//! exchange (mail, seals, jobs, rows, profiles, capacity, quota): each is one or two socket
//! requests; `wait` and `ask` block on the socket's events, never polling.
use crate::cli::{absolute, call, connect, get};
use crate::{release_record, Error, Result};
use clap::{Arg, ArgAction, ArgMatches, Command};
use serde_json::{json, Value};
use std::{path::Path, time::Duration};

/// The verbs this module answers, as `qream <verb>`.
const VERBS: &[&str] = &[
    "chief",
    "controller",
    "deploy",
    "status",
    "accept",
    "read",
    "send",
    "wait",
    "msg",
    "broadcast",
    "inbox",
    "mail",
    "ack",
    "ask",
    "answer",
    "job",
    "submit",
    "row",
    "gapfill",
    "profile",
    "capacity",
    "quota",
    "fence",
    "events",
    "release-record",
    "notify",
];
pub fn owns(verb: &str) -> bool {
    VERBS.contains(&verb)
}

fn o(name: &'static str) -> Arg {
    Arg::new(name).long(name)
}
fn flag(name: &'static str) -> Arg {
    Arg::new(name).long(name).action(ArgAction::SetTrue)
}
fn task_arg() -> Arg {
    o("task").help("the task, else $QREAM_TASK")
}
fn text_args(c: Command) -> Command {
    c.arg(o("text")).arg(o("file")).arg(o("id"))
}

pub fn commands() -> Vec<Command> {
    let name = || Arg::new("name");
    vec![
        Command::new("chief")
            .about("Open the campaign's seat and invoke its chief; omitted choices use its profile")
            .arg(name())
            .arg(o("kind").value_parser(["claude", "codex", "opencode"]))
            .arg(o("model"))
            .arg(o("effort"))
            .arg(o("resume")),
        ["start", "trigger", "status"].into_iter().fold(
            Command::new("controller").subcommand_required(true),
            |c, v| c.subcommand(Command::new(v).arg(name())),
        ),
        Command::new("deploy").arg(name()).arg(flag("dry-run")),
        Command::new("status").arg(name()),
        Command::new("accept")
            .arg(task_arg())
            .arg(o("evidence"))
            .arg(o("by")),
        Command::new("read").arg(task_arg()).arg(o("lines")),
        text_args(Command::new("send").arg(task_arg())),
        Command::new("wait")
            .arg(task_arg())
            .arg(o("to").help("the state word to wait for, e.g. awaiting_review or finished"))
            .arg(o("mail"))
            .arg(o("since"))
            .arg(o("timeout")),
        text_args(
            // The skill writes `qream msg <campaign>/worker/<task> --run <run> --subject <words>`.
            Command::new("msg")
                .arg(Arg::new("recipient").required_unless_present("to"))
                .arg(o("to"))
                .arg(o("from"))
                .arg(o("run"))
                .arg(o("subject")),
        ),
        text_args(Command::new("broadcast")),
        Command::new("inbox").arg(o("to")),
        Command::new("mail")
            .arg(o("to"))
            .arg(o("id").required(true)),
        Command::new("ack").arg(o("to")).arg(o("id").required(true)),
        text_args(
            Command::new("ask")
                .arg(o("to").required(true))
                .arg(o("timeout")),
        ),
        Command::new("answer")
            .arg(o("to").required(true))
            .arg(o("id").required(true))
            .arg(o("text").required(true)),
        Command::new("job")
            .arg(
                Arg::new("action")
                    .value_parser(["start", "list", "stop"])
                    .required(true),
            )
            .arg(task_arg())
            .arg(Arg::new("job").long("name").help("the job's name"))
            .arg(o("cwd"))
            .arg(
                Arg::new("command")
                    .num_args(0..)
                    .last(true)
                    .allow_hyphen_values(true),
            ),
        Command::new("submit")
            .arg(o("report").required(true))
            .arg(task_arg())
            .arg(o("candidate"))
            .arg(flag("seal")),
        Command::new("row").subcommand_required(true).subcommand(
            Command::new("check")
                .arg(o("id").required(true))
                .arg(o("evidence").required(true))
                .arg(flag("closing")),
        ),
        // The skill writes `qream gapfill TASK now|after --reason <reason>`.
        Command::new("gapfill")
            .arg(task_arg())
            .arg(Arg::new("task_word").value_name("TASK"))
            .arg(
                Arg::new("when_word")
                    .value_name("WHEN")
                    .value_parser(["now", "after"]),
            )
            .arg(
                o("when")
                    .value_parser(["now", "after"])
                    .default_value("now"),
            )
            .arg(o("reason").required(true))
            .arg(o("rows")),
        Command::new("profile")
            .arg(o("worker").help("harness/model/effort"))
            .arg(o("orchestrator").help("harness/model/effort"))
            .arg(o("wave"))
            .arg(o("profile").help("harness/model/effort for --wave")),
        Command::new("capacity"),
        Command::new("quota").arg(o("kind").required(true)),
        Command::new("fence").arg(task_arg()).arg(flag("release")),
        Command::new("events").arg(o("since")),
        Command::new("release-record")
            .arg(o("source").required(true))
            .arg(o("out").required(true))
            .arg(o("release-id").required(true))
            .arg(o("command").action(ArgAction::Append).required(true)),
        Command::new("notify").arg(Arg::new("message").num_args(1..).required(true)),
    ]
}

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}
/// The campaign: positional, `--campaign`, `$QREAM_CAMPAIGN`, or the registered root
/// that holds `path`.
fn campaign(m: &ArgMatches, top: &ArgMatches, path: Option<&str>) -> Result<String> {
    if let Some(name) = m
        .try_get_one::<String>("name")
        .ok()
        .flatten()
        .cloned()
        .or_else(|| get(top, "campaign").map(str::to_owned))
        .or_else(|| env("QREAM_CAMPAIGN"))
    {
        return Ok(name);
    }
    if let Some(path) = path {
        let path = absolute(path)?;
        if let Some(found) = call("campaigns.list", json!({}))?
            .as_array()
            .into_iter()
            .flatten()
            .filter(|c| {
                c["root"]
                    .as_str()
                    .is_some_and(|r| Path::new(&path).starts_with(r))
            })
            .max_by_key(|c| c["root"].as_str().map_or(0, str::len))
        {
            return Ok(found["name"].as_str().unwrap_or("").to_owned());
        }
    }
    Err(Error::Refused(
        "Name the campaign (or pass --campaign)".into(),
    ))
}
/// The task: `--task`, `$QREAM_TASK`, or a report named `TNN_…`.
fn task(m: &ArgMatches, report: Option<&str>) -> Result<String> {
    m.try_get_one::<String>("task")
        .ok()
        .flatten()
        .cloned()
        .or_else(|| env("QREAM_TASK"))
        .or_else(|| {
            let name = Path::new(report?).file_name()?.to_str()?;
            let id = name.split('_').next()?;
            (id.starts_with('T') && id[1..].chars().all(|c| c.is_ascii_digit()))
                .then(|| id.to_owned())
        })
        .ok_or_else(|| Error::Refused("Name the task (or pass --task)".into()))
}
fn body(m: &ArgMatches) -> Result<Value> {
    Ok(match (get(m, "text"), get(m, "file")) {
        (Some(text), _) => json!({"body":text}),
        (None, Some(file)) => json!({"file":absolute(file)?}),
        _ => return Err(Error::Refused("Give --text or --file".into())),
    })
}
fn seconds(m: &ArgMatches, default: u64) -> Result<Duration> {
    Ok(Duration::from_secs(match get(m, "timeout") {
        Some(t) => t
            .parse()
            .map_err(|_| Error::Refused("--timeout needs seconds".into()))?,
        None => default,
    }))
}
fn merge(mut base: Value, extra: Value) -> Value {
    if let (Some(b), Value::Object(e)) = (base.as_object_mut(), extra) {
        b.extend(e);
    }
    base
}

/// Run one verb; the value printed and the exit code (0 done, 1 refused).
pub fn run(verb: &str, m: &ArgMatches, top: &ArgMatches) -> Result<(Value, i32)> {
    let c = || campaign(m, top, None);
    let value = match verb {
        "chief" => {
            let mut args = json!({"name":c()?,"chief":true});
            for key in ["kind", "model", "effort", "resume"] {
                if let Some(value) = get(m, key) {
                    args[key] = json!(value);
                }
            }
            call("seat.invoke", args)?
        }
        "controller" => {
            let (action, s) = m
                .subcommand()
                .ok_or_else(|| Error::Refused("start, trigger or status".into()))?;
            let name = campaign(s, top, None)?;
            let cmd = match action {
                "trigger" => "campaign.trigger",
                "start" => "controller.start",
                _ => "controller.status",
            };
            call(cmd, json!({"campaign":name}))?
        }
        "deploy" => {
            if !m.get_flag("dry-run") {
                return Ok((
                    json!({"error":{"code":"refused","message":"A campaign starts only on Trigger; deploy shows what Trigger would start with --dry-run"}}),
                    1,
                ));
            }
            call("deploy.dry_run", json!({"campaign":c()?}))?
        }
        "status" => call("run.status", json!({"campaign":c()?}))?,
        "accept" => call(
            "run.accept",
            json!({"campaign":c()?,"task":task(m, None)?,"evidence":get(m,"evidence"),"by":get(m,"by").unwrap_or("chief")}),
        )?,
        "read" => call(
            "run.read",
            json!({"campaign":c()?,"task":task(m, None)?,"lines":get(m,"lines").and_then(|l| l.parse::<u64>().ok()).unwrap_or(50)}),
        )?,
        "send" => call(
            "run.send",
            merge(
                json!({"campaign":c()?,"task":task(m, None)?,"id":get(m,"id")}),
                body(m)?,
            ),
        )?,
        "msg" => {
            let mut text = body(m)?;
            // A subject heads the body, so the owner reads what the message is before its log.
            if let (Some(subject), Some(file)) = (get(m, "subject"), get(m, "file")) {
                let log = std::fs::read_to_string(file).map_err(|source| Error::Io {
                    path: file.into(),
                    source,
                })?;
                text = json!({"body":format!("{subject}\n\n{log}")});
            } else if let (Some(subject), Some(words)) = (get(m, "subject"), get(m, "text")) {
                text = json!({"body":format!("{subject}\n\n{words}")});
            }
            let to = get(m, "to").or_else(|| get(m, "recipient"));
            call(
                "mail.send",
                merge(
                    json!({"campaign":c()?,"to":to,"from":get(m,"from").map(str::to_owned).or_else(|| env("QREAM_TASK")),"id":get(m,"id"),"run":get(m,"run")}),
                    text,
                ),
            )?
        }
        "broadcast" => {
            let text = get(m, "text").ok_or_else(|| Error::Refused("Give --text".into()))?;
            call(
                "mail.broadcast",
                json!({"campaign":c()?,"body":text,"id":get(m,"id")}),
            )?
        }
        "inbox" => {
            let to = get(m, "to")
                .map(str::to_owned)
                .or_else(|| env("QREAM_TASK"))
                .unwrap_or_else(|| "chief".into());
            call("mail.inbox", json!({"campaign":c()?,"to":to}))?
        }
        "mail" | "ack" => {
            let to = get(m, "to")
                .map(str::to_owned)
                .or_else(|| env("QREAM_TASK"))
                .unwrap_or_else(|| "chief".into());
            let cmd = if verb == "mail" {
                "mail.read"
            } else {
                "mail.ack"
            };
            call(cmd, json!({"campaign":c()?,"to":to,"id":get(m,"id")}))?
        }
        "answer" => call(
            "mail.answer",
            json!({"campaign":c()?,"to":get(m,"to"),"id":get(m,"id"),"body":get(m,"text")}),
        )?,
        "ask" => {
            let name = c()?;
            let mut client = connect()?;
            client.call("subscribe", json!({"topics":["mail"]}))?;
            let sent = client.call(
                "mail.ask",
                merge(
                    json!({"campaign":name,"to":get(m,"to"),"from":env("QREAM_TASK"),"id":get(m,"id")}),
                    body(m)?,
                ),
            )?;
            let id = sent["id"].as_str().unwrap_or("").to_owned();
            let timeout = seconds(m, 600)?;
            loop {
                let event = client.event(timeout)?;
                if event["event"] == "mail"
                    && event["data"]["id"] == id.as_str()
                    && event["data"]["answered"] == true
                {
                    break client
                        .call("mail.inbox", json!({"campaign":name,"to":sent["to"]}))?
                        .as_array()
                        .and_then(|all| all.iter().find(|x| x["id"] == id.as_str()).cloned())
                        .unwrap_or(Value::Null);
                }
            }
        }
        "wait" => wait(m, top)?,
        "job" => {
            let name = c()?;
            let action = get(m, "action").unwrap_or("list");
            match action {
                "start" => {
                    let command: Vec<String> = m
                        .get_many::<String>("command")
                        .map(|v| v.cloned().collect())
                        .unwrap_or_default();
                    let cwd = match get(m, "cwd") {
                        Some(cwd) => absolute(cwd)?,
                        None => absolute(".")?,
                    };
                    call(
                        "job.start",
                        json!({"campaign":name,"task":task(m, None)?,"name":get(m,"job"),"cwd":cwd,"command":command}),
                    )?
                }
                "stop" => call(
                    "job.stop",
                    json!({"campaign":name,"task":task(m, None)?,"name":get(m,"job")}),
                )?,
                _ => call(
                    "job.list",
                    json!({"campaign":name,"task":get(m,"task").map(str::to_owned).or_else(|| env("QREAM_TASK"))}),
                )?,
            }
        }
        "submit" => {
            let report = absolute(get(m, "report").unwrap_or(""))?;
            // The daemon's mask gate reads the report first; a missing campaign or task is
            // refused there, after the gate.
            let name = campaign(m, top, Some(&report)).ok();
            let mut args = json!({"campaign":name,"task":task(m, Some(&report)).ok(),"report":report,"seal":m.get_flag("seal")});
            if let Some(candidate) = get(m, "candidate") {
                args["candidate"] = json!(absolute(candidate)?);
            }
            call("submit", args)?
        }
        "row" => {
            let (_, s) = m
                .subcommand()
                .ok_or_else(|| Error::Refused("row check".into()))?;
            call(
                "row.check",
                json!({"campaign":campaign(s, top, None).ok(),"id":get(s,"id"),"evidence":get(s,"evidence"),"closing":s.get_flag("closing")}),
            )?
        }
        "gapfill" => call(
            "gapfill",
            json!({"campaign":c()?,"task":get(m,"task_word").map_or_else(|| task(m, None), |t| Ok(t.to_owned()))?,"when":get(m,"when_word").or_else(|| get(m,"when")),"reason":get(m,"reason"),"rows":get(m,"rows")}),
        )?,
        "profile" => {
            let name = c()?;
            let setting = ["worker", "orchestrator", "profile"]
                .iter()
                .any(|k| get(m, k).is_some());
            if setting {
                let mut args = json!({"campaign":name,"worker":get(m,"worker"),"orchestrator":get(m,"orchestrator"),"profile":get(m,"profile")});
                if let Some(wave) = get(m, "wave") {
                    args["wave"] = json!(wave
                        .parse::<u64>()
                        .map_err(|_| Error::Refused("--wave needs a number".into()))?);
                }
                call("profile.set", args)?
            } else {
                call("profile.get", json!({"campaign":name}))?
            }
        }
        "capacity" => call(
            "capacity",
            json!({"campaign":get(top,"campaign").map(str::to_owned).or_else(|| env("QREAM_CAMPAIGN"))}),
        )?,
        "quota" => call("quota", json!({"campaign":c()?,"kind":get(m,"kind")}))?,
        "fence" => call(
            "fence",
            json!({"campaign":c()?,"task":get(m,"task"),"release":m.get_flag("release")}),
        )?,
        "events" => call(
            "events",
            json!({"campaign":c()?,"since":get(m,"since").and_then(|s| s.parse::<u64>().ok()).unwrap_or(0)}),
        )?,
        "release-record" => {
            let commands = m
                .get_many::<String>("command")
                .map(|v| v.cloned().collect())
                .unwrap_or_default();
            serde_json::to_value(release_record::write(
                Path::new(get(m, "source").unwrap_or("")),
                Path::new(get(m, "out").unwrap_or("")),
                get(m, "release-id").unwrap_or(""),
                commands,
            )?)?
        }
        "notify" => {
            let words: Vec<String> = m
                .get_many::<String>("message")
                .map(|v| v.cloned().collect())
                .unwrap_or_default();
            call("notify", json!({"message":words.join(" ")}))?
        }
        _ => return Err(Error::Unimplemented),
    };
    Ok((value, 0))
}

/// Block on the socket until a matching `task` or `mail` event; `--since N` first reads
/// the ledger from line N so an event that already happened answers at once.
fn wait(m: &ArgMatches, top: &ArgMatches) -> Result<Value> {
    let name = campaign(m, top, None)?;
    let task = get(m, "task").map(str::to_owned).or_else(|| {
        (get(m, "mail").is_none() && get(m, "to").is_none())
            .then(|| env("QREAM_TASK"))
            .flatten()
    });
    let to = get(m, "to").map(str::to_owned);
    let mail = get(m, "mail").map(str::to_owned);
    let matches = |data: &Value| -> bool {
        if data["campaign"] != name.as_str() {
            return false;
        }
        if let Some(id) = &mail {
            return data["id"] == id.as_str();
        }
        let task_ok = match &task {
            Some(t) => data["task"] == t.as_str(),
            None => true,
        };
        let to_ok = to.as_ref().is_none_or(|w| {
            data["to"]
                .as_str()
                .is_some_and(|x| x.starts_with(w.as_str()))
        });
        task_ok && to_ok
    };
    let mut client = connect()?;
    client.call("subscribe", json!({"topics":["task","mail"]}))?;
    // One line on stderr once subscribed, so a caller can act knowing nothing is missed.
    eprintln!("{}", json!({"waiting":true,"campaign":name}));
    if let Some(since) = get(m, "since").and_then(|s| s.parse::<u64>().ok()) {
        let past = client.call("events", json!({"campaign":name,"since":since}))?;
        if let Some(hit) = past["events"]
            .as_array()
            .and_then(|all| all.iter().find(|e| matches(e)))
        {
            return Ok(json!({"event":"task","data":hit,"from":"ledger"}));
        }
    }
    let timeout = seconds(m, 3600)?;
    loop {
        let event = client.event(timeout)?;
        if matches(&event["data"]) {
            return Ok(event);
        }
    }
}
