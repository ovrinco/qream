//! The command tree (CONTRACTS.md §6) and its thin handlers: every working verb is one
//! or two socket requests; file-only checks call the engine library in-process.
use clap::{Arg, ArgAction, ArgMatches, Command};
use serde_json::{json, Map, Value};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::engine::{
    api::encode_base64,
    client::{self, Client},
    daemon::Paths,
};
use crate::{build_identity, release_record, Error, Result};

const UNBUILT: &str = "wave 3";
/// §6 verbs a later wave builds; they parse any arguments and answer unimplemented.
const LATER: &[&str] = &[];
const LATER_GROUPS: &[(&str, &[&str])] = &[];

fn rest(name: &'static str) -> Command {
    Command::new(name).arg(
        Arg::new("args")
            .num_args(0..)
            .allow_hyphen_values(true)
            .trailing_var_arg(true),
    )
}
fn opt(name: &'static str) -> Arg {
    Arg::new(name).long(name)
}
fn need(name: &'static str) -> Arg {
    Arg::new(name).long(name).required(true)
}
fn pos(name: &'static str) -> Arg {
    Arg::new(name)
}
fn group(name: &'static str, verbs: Vec<Command>) -> Command {
    verbs
        .into_iter()
        .fold(Command::new(name).subcommand_required(true), |c, v| {
            c.subcommand(v)
        })
}

pub fn command(name: &'static str) -> Command {
    let mut command = Command::new(name)
        .disable_version_flag(true)
        .arg(
            Arg::new("version")
                .long("version")
                .action(ArgAction::SetTrue),
        )
        .arg(Arg::new("campaign").long("campaign").global(true));
    if name == "qream" {
        command = command
            .about("QREAM: campaigns, terminal sessions and the tracker store, through one daemon")
            .subcommand(group(
                "daemon",
                vec![
                    Command::new("start"),
                    Command::new("stop")
                        .arg(opt("keep").help("sessions to keep for the next start, comma separated"))
                        .arg(Arg::new("keep-all").long("keep-all").action(ArgAction::SetTrue)),
                    Command::new("status"),
                ],
            ))
            .subcommand(Command::new("campaigns"))
            .subcommand(group(
                "campaign",
                vec![
                    Command::new("up")
                        .arg(pos("name"))
                        .arg(need("root"))
                        .arg(opt("state"))
                        .arg(opt("session"))
                        .arg(opt("mode"))
                        .arg(opt("title")),
                    Command::new("down").arg(pos("name")),
                    Command::new("trigger").arg(pos("name")),
                ],
            ))
            .subcommand(group(
                "setup",
                vec![
                    Command::new("new")
                        .arg(need("root"))
                        .arg(need("name"))
                        .arg(opt("rows"))
                        .arg(opt("groups")),
                    Command::new("show").arg(pos("root").required(true)),
                    Command::new("check").arg(pos("root").required(true)),
                    Command::new("add-directive")
                        .arg(pos("root").required(true))
                        .arg(need("wave"))
                        .arg(need("task"))
                        .arg(need("file")),
                    Command::new("move")
                        .arg(pos("root").required(true))
                        .arg(need("task"))
                        .arg(need("wave")),
                    Command::new("manifest")
                        .arg(pos("root").required(true))
                        .arg(need("wave"))
                        .arg(need("kind"))
                        .arg(need("model"))
                        .arg(need("effort"))
                        .arg(opt("tasks")),
                    Command::new("confirm")
                        .arg(pos("root").required(true))
                        .arg(opt("last-wave"))
                        .arg(opt("expected-close"))
                        .arg(opt("by")),
                ],
            ))
            .subcommand(
                Command::new("lint")
                    .arg(pos("path"))
                    .arg(opt("manifest"))
                    .arg(opt("campaign-root"))
                    .arg(opt("rules"))
                    .arg(opt("charter"))
                    .arg(opt("lanes")),
            )
            .subcommand(Command::new("gui").about("Open the campaign window on this daemon"))
            .subcommand(Command::new("states").arg(pos("name")))
            .subcommand(
                Command::new("control")
                    .about("Interrupt, pause, kill or release a campaign, wave:N, worker:TNN, orchestrator or controller")
                    .arg(pos("action").required(true))
                    .arg(pos("target").default_value("campaign")),
            )
            .subcommand(Command::new("recover").about("Resume every halted agent in its own history"))
            .subcommand(Command::new("refresh").arg(need("task")))
            .subcommand(
                Command::new("leftovers")
                    .arg(Arg::new("kill").long("kill").action(ArgAction::SetTrue)),
            )
            .subcommand(Command::new("close").arg(pos("name")))
            .subcommand(group(
                "harness",
                vec![
                    Command::new("up").arg(pos("name")).arg(opt("cwd")),
                    Command::new("status").arg(pos("name")),
                ],
            ))
            .subcommand(group(
                "faults",
                vec![
                    Command::new("add")
                        .arg(need("area"))
                        .arg(need("fault"))
                        .arg(need("effect"))
                        .arg(need("source"))
                        .arg(opt("status"))
                        .arg(opt("fix")),
                    Command::new("amend")
                        .arg(pos("id").required(true))
                        .arg(need("status"))
                        .arg(opt("fix")),
                    Command::new("list"),
                ],
            ))
            .subcommand(group(
                "replay",
                ["record", "list", "summary", "events"]
                    .map(|verb| {
                        Command::new(verb)
                            .arg(opt("root"))
                            .arg(opt("task"))
                            .arg(opt("from"))
                            .arg(opt("to"))
                    })
                    .to_vec(),
            ))
            .subcommand(
                rest("term")
                    .about("Terminal layer: `qream term pane text --pane w1:p1 --lines 20` sends term.pane.text"),
            );
        for verb in crate::verbs::commands() {
            command = command.subcommand(verb);
        }
        for &verb in LATER {
            command = command.subcommand(rest(verb));
        }
        for &(name, verbs) in LATER_GROUPS {
            command = command.subcommand(group(name, verbs.iter().map(|v| rest(v)).collect()));
        }
    } else if name == "qream-install" {
        command = command.subcommand(
            Command::new("release-record")
                .arg(Arg::new("source").long("source").required(true))
                .arg(Arg::new("out").long("out").required(true))
                .arg(Arg::new("release-id").long("release-id").required(true))
                .arg(
                    Arg::new("command")
                        .long("command")
                        .action(ArgAction::Append)
                        .required(true),
                ),
        );
    }
    command
}

pub(crate) fn print_json(value: &impl serde::Serialize) -> Result<()> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    serde_json::to_writer(&mut out, value)?;
    out.write_all(b"\n").map_err(|source| Error::Io {
        path: "<stdout>".into(),
        source,
    })
}

pub(crate) fn get<'a>(m: &'a ArgMatches, key: &str) -> Option<&'a str> {
    m.try_get_one::<String>(key)
        .ok()
        .flatten()
        .map(String::as_str)
}
fn number(m: &ArgMatches, key: &str) -> Result<Value> {
    let text = get(m, key).unwrap_or("");
    text.parse::<u64>()
        .map(Value::from)
        .map_err(|_| Error::Refused(format!("--{key} needs a number, not {text:?}")))
}
pub(crate) fn absolute(text: &str) -> Result<String> {
    let path = Path::new(text);
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()
            .map_err(|source| Error::Io {
                path: ".".into(),
                source,
            })?
            .join(path)
    };
    Ok(path.display().to_string())
}
fn root(m: &ArgMatches) -> Result<String> {
    absolute(get(m, "root").unwrap_or(""))
}
/// The campaign named positionally or by `--campaign`.
fn campaign(m: &ArgMatches, top: &ArgMatches) -> Result<String> {
    get(m, "name")
        .or_else(|| get(m, "campaign"))
        .or_else(|| get(top, "campaign"))
        .map(str::to_owned)
        .ok_or_else(|| Error::Refused("Name the campaign (or pass --campaign)".into()))
}
fn qreamd() -> Result<PathBuf> {
    let exe = std::env::current_exe().map_err(|source| Error::Io {
        path: "<current executable>".into(),
        source,
    })?;
    Ok(exe.with_file_name("qreamd"))
}
/// The window's `shell.qml`: `QREAM_UI`, else `ui/` beside the release's `bin/`.
fn window_shell() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("QREAM_UI") {
        return Ok(PathBuf::from(dir).join("shell.qml"));
    }
    let bin = qreamd()?;
    let release = bin
        .parent()
        .and_then(Path::parent)
        .unwrap_or(Path::new("/"));
    let shell = release.join("ui").join("shell.qml");
    if shell.is_file() {
        Ok(shell)
    } else {
        Err(Error::Refused(format!(
            "No window beside this build ({}); set QREAM_UI to the ui folder",
            shell.display()
        )))
    }
}
/// Open the window in its own session, including when the engine is stopped.
fn gui() -> Result<Value> {
    let shell = window_shell()?;
    let child = std::process::Command::new("setsid")
        .arg("-f")
        .arg("qs")
        .arg("--path")
        .arg(&shell)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|source| Error::Io {
            path: "qs".into(),
            source,
        })?;
    if !child.success() {
        return Err(Error::Refused("The window did not start (qs)".into()));
    }
    Ok(json!({"opened": shell}))
}
pub(crate) fn connect() -> Result<Client> {
    Ok(Client::connect_or_explain(&Paths::from_env()?)?)
}
/// File checks only read the selected store; they never create or migrate it.
fn file_check_store() -> Result<crate::engine::store::Store> {
    let path = std::env::var_os("QREAM_STORE")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .map(Ok)
        .unwrap_or_else(crate::engine::daemon::default_store)?;
    Ok(crate::engine::store::Store::open_read_only(&path).map_err(crate::engine::Error::from)?)
}
pub fn call(cmd: &str, args: Value) -> Result<Value> {
    Ok(connect()?.call(cmd, args)?)
}

/// `qream term pane text --pane w1:p1 --lines 20` → `term.pane.text {pane, lines}`.
/// Values parse as JSON where they can; `--text` is sent as `bytes_b64`.
fn term_request(words: &[String]) -> Result<(String, Value)> {
    let mut path = vec!["term".to_owned()];
    let mut args = Map::new();
    let mut i = 0;
    while i < words.len() {
        if let Some(key) = words[i].strip_prefix("--") {
            let value = words
                .get(i + 1)
                .ok_or_else(|| Error::Refused(format!("--{key} needs a value")))?;
            if key == "text" {
                args.insert("bytes_b64".into(), json!(encode_base64(value.as_bytes())));
            } else {
                let parsed = serde_json::from_str::<Value>(value)
                    .ok()
                    .filter(|v| !v.is_string())
                    .unwrap_or_else(|| json!(value));
                args.insert(key.replace('-', "_"), parsed);
            }
            i += 2;
        } else {
            path.push(words[i].clone());
            i += 1;
        }
    }
    if path.len() == 1 {
        return Err(Error::Refused(
            "Name the terminal command, e.g. `term sessions`".into(),
        ));
    }
    Ok((path.join("."), Value::Object(args)))
}

fn execute(name: &'static str) -> Result<i32> {
    let matches = match command(name).try_get_matches() {
        Ok(matches) => matches,
        Err(error)
            if matches!(
                error.kind(),
                clap::error::ErrorKind::DisplayHelp
                    | clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
            ) =>
        {
            error.print().map_err(|source| Error::Io {
                path: "<stdout>".into(),
                source,
            })?;
            return Ok(0);
        }
        Err(error) => return Err(error.into()),
    };
    if matches.get_flag("version") {
        print_json(&build_identity())?;
        return Ok(0);
    }
    if name == "qream-install" {
        if let Some(("release-record", args)) = matches.subcommand() {
            let get = |key: &str| -> Result<&str> {
                args.get_one::<String>(key)
                    .map(String::as_str)
                    .ok_or(Error::Unimplemented)
            };
            let commands = args
                .get_many::<String>("command")
                .map(|values| values.cloned().collect())
                .unwrap_or_default();
            let record = release_record::write(
                Path::new(get("source")?),
                Path::new(get("out")?),
                get("release-id")?,
                commands,
            )?;
            print_json(&record)?;
            return Ok(0);
        }
        return unbuilt();
    }
    let Some((verb, m)) = matches.subcommand() else {
        command(name).print_help().map_err(|source| Error::Io {
            path: "<stdout>".into(),
            source,
        })?;
        return Ok(0);
    };
    let value = match (verb, m.subcommand()) {
        ("daemon", Some(("start", _))) => {
            let (mut client, started) = Client::start(&Paths::from_env()?, &qreamd()?)?;
            let ping = client.call("ping", json!({}))?;
            json!({"started":started,"hello":client.hello["data"],"ping":ping})
        }
        ("daemon", Some(("status", _))) => {
            let client = connect()?;
            json!({"running":true,"hello":client.hello["data"]})
        }
        ("daemon", Some(("stop", s))) => {
            let mut client = connect()?;
            let keep: Vec<&str> = get(s, "keep")
                .map(|k| k.split(',').filter(|w| !w.is_empty()).collect())
                .unwrap_or_default();
            client.call(
                "daemon.stop",
                json!({"keep":keep,"keep_all":s.get_flag("keep-all")}),
            )?
        }
        ("campaigns", _) => call("campaigns.list", json!({}))?,
        ("campaign", Some(("up", s))) => {
            let mut args = json!({"name":campaign(s, &matches)?,"root":root(s)?});
            for key in ["state", "session", "mode", "title"] {
                if let Some(v) = get(s, key) {
                    args[key] = json!(v);
                }
            }
            call("campaign.up", args)?
        }
        ("campaign", Some(("down", s))) => {
            call("campaign.down", json!({"name":campaign(s, &matches)?}))?
        }
        ("setup", Some(("new", s))) => call(
            "setup.new",
            json!({"root":root(s)?,"name":get(s,"name"),"rows":get(s,"rows").unwrap_or(""),"groups":get(s,"groups").unwrap_or("")}),
        )?,
        ("setup", Some(("show", s))) => call("setup.show", json!({"root":root(s)?}))?,
        ("setup", Some(("check", s))) => {
            let store = file_check_store()?;
            let checked =
                serde_json::to_value(crate::engine::setup::check(Path::new(&root(s)?), &store)?)?;
            print_json(&checked)?;
            return Ok(i32::from(checked["ok"] != true));
        }
        ("setup", Some(("add-directive", s))) => call(
            "setup.add_directive",
            json!({"root":root(s)?,"wave":number(s,"wave")?,"task":get(s,"task"),"file":absolute(get(s,"file").unwrap_or(""))?}),
        )?,
        ("setup", Some(("move", s))) => call(
            "setup.move",
            json!({"root":root(s)?,"task":get(s,"task"),"wave":number(s,"wave")?}),
        )?,
        ("setup", Some(("manifest", s))) => {
            let mut args = json!({"root":root(s)?,"wave":number(s,"wave")?,"kind":get(s,"kind"),"model":get(s,"model"),"effort":get(s,"effort")});
            if let Some(tasks) = get(s, "tasks") {
                args["tasks"] = json!(absolute(tasks)?);
            }
            call("setup.manifest", args)?
        }
        ("setup", Some(("confirm", s))) => {
            let mut args = json!({"root":root(s)?});
            if get(s, "last-wave").is_some() {
                args["close"] = json!({"last_wave":number(s,"last-wave")?,"expected_close":get(s,"expected-close").unwrap_or(""),"by":get(s,"by").unwrap_or("planning-agent")});
            }
            let confirmed = call("setup.confirm", args)?;
            print_json(&confirmed)?;
            return Ok(i32::from(confirmed["ok"] == false));
        }
        ("lint", _) => {
            let target = get(m, "manifest")
                .or_else(|| get(m, "path"))
                .ok_or_else(|| {
                    Error::Refused("Give a directive, wave, root or --manifest".into())
                })?;
            let mut args = json!({"path":absolute(target)?});
            for (flag, key) in [
                ("campaign-root", "campaign_root"),
                ("rules", "rules"),
                ("charter", "charter"),
                ("lanes", "lanes"),
            ] {
                if let Some(v) = get(m, flag) {
                    args[key] = json!(absolute(v)?);
                }
            }
            let store = file_check_store()?;
            let mut options = crate::engine::rules::LintOptions::new(&store)?;
            options.campaign_root = args["campaign_root"].as_str().map(PathBuf::from);
            options.charter = args["charter"].as_str().map(PathBuf::from);
            options.lanes = args["lanes"].as_str().map(PathBuf::from);
            if let Some(rules) = args["rules"].as_str() {
                options.extra_rules(Path::new(rules))?;
            }
            let findings = serde_json::to_value(crate::engine::rules::lint(
                Path::new(args["path"].as_str().ok_or(Error::Unimplemented)?),
                &options,
            )?)?;
            print_json(&findings)?;
            return Ok(i32::from(findings["blocking"].as_u64().unwrap_or(0) > 0));
        }
        ("states", _) => {
            let name = campaign(m, &matches)?;
            let mut state = call("states", json!({"campaign":name}))?;
            state["chief"] = call("seat.get", json!({"name":name}))?;
            state
        }
        ("control", _) => call(
            "campaign.control",
            json!({"name":campaign(m, &matches)?,"action":get(m, "action"),"target":get(m, "target")}),
        )?,
        ("recover", _) => {
            let mut args = json!({});
            if let Some(name) = get(&matches, "campaign") {
                args["campaign"] = json!(name);
            }
            call("recover", args)?
        }
        ("refresh", _) => call(
            "refresh",
            json!({"campaign":campaign(m, &matches)?,"task":get(m, "task")}),
        )?,
        ("leftovers", _) => {
            let mut args = json!({"kill":m.get_flag("kill")});
            if let Some(name) = get(&matches, "campaign") {
                args["campaign"] = json!(name);
            }
            call("leftovers", args)?
        }
        ("close", _) => call("close.get", json!({"campaign":campaign(m, &matches)?}))?,
        ("harness", Some(("up", s))) => {
            let name = campaign(s, &matches)?;
            let mut client = connect()?;
            let cwd = match get(s, "cwd") {
                Some(cwd) => PathBuf::from(absolute(cwd)?),
                None => client
                    .call("campaign.get", json!({"name":name}))
                    .ok()
                    .and_then(|c| c["root"].as_str().map(PathBuf::from))
                    .map_or_else(|| Paths::from_env().map(|p| p.home), Ok)?,
            };
            client::harness_up(&mut client, &name, &cwd)?
        }
        ("gui", _) => gui()?,
        ("harness", Some(("status", s))) => {
            client::harness_status(&mut connect()?, &campaign(s, &matches)?)?
        }
        ("campaign", Some(("trigger", s))) => {
            call("campaign.trigger", json!({"name":campaign(s, &matches)?}))?
        }
        (verb, _) if crate::verbs::owns(verb) => {
            let (value, exit) = crate::verbs::run(verb, m, &matches)?;
            print_json(&value)?;
            return Ok(exit);
        }
        ("faults", Some(("add", s))) => {
            let mut args = json!({});
            for key in ["area", "fault", "effect", "source", "status", "fix"] {
                if let Some(v) = get(s, key) {
                    args[key] = json!(v);
                }
            }
            call("faults.add", args)?
        }
        ("faults", Some(("amend", s))) => call(
            "faults.amend",
            json!({"id":get(s,"id"),"status":get(s,"status"),"fix":get(s,"fix").unwrap_or("")}),
        )?,
        ("faults", Some(("list", _))) => call("faults.list", json!({}))?,
        ("replay", Some((verb, s))) => {
            let mut args = match get(s, "root") {
                Some(root) => json!({"root":absolute(root)?}),
                None => json!({"campaign":get(s, "campaign")
                    .or_else(|| get(&matches, "campaign"))
                    .ok_or_else(|| Error::Refused("Name the campaign (--campaign) or its --root".into()))?}),
            };
            for key in ["task", "from", "to"] {
                if let Some(v) = get(s, key) {
                    args[key] = json!(v);
                }
            }
            call(&format!("replay.{verb}"), args)?
        }
        ("term", _) => {
            let words: Vec<String> = m
                .get_many::<String>("args")
                .map(|v| v.cloned().collect())
                .unwrap_or_default();
            let (cmd, args) = term_request(&words)?;
            call(&cmd, args)?
        }
        _ => return unbuilt(),
    };
    print_json(&value)?;
    Ok(0)
}

fn unbuilt() -> Result<i32> {
    print_json(&json!({"error":{"code":"unimplemented","message":UNBUILT}}))?;
    Ok(2)
}

/// `qream hook`: a harness's turn hook (Codex or Claude Code). It reads the hook's JSON on stdin and
/// records the task's turn in `<campaign root>/state/turns/<task>.json` (active from a submitted
/// prompt to the turn's end), with the harness's own session id. It answers nothing and always exits
/// 0, so it can never hold up or block the agent; outside a QREAM worker it does nothing.
pub fn hook() -> i32 {
    use std::io::Read;
    let (Some(root), Some(task)) = (
        std::env::var_os("QREAM_ROOT"),
        std::env::var("QREAM_TASK").ok(),
    ) else {
        return 0;
    };
    let mut input = String::new();
    let _ = std::io::stdin().take(1 << 20).read_to_string(&mut input);
    let event: Value = serde_json::from_str(&input).unwrap_or(Value::Null);
    let name = event["hook_event_name"].as_str().unwrap_or("").to_owned();
    let active = match name.as_str() {
        "UserPromptSubmit" => Some(true),
        "Stop" | "Interrupt" | "SessionEnd" => Some(false),
        "SessionStart" => None,
        _ => return 0,
    };
    let dir = Path::new(&root).join("state/turns");
    let path = dir.join(format!("{task}.json"));
    let previous: Value = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Value::Null);
    let record = json!({
        "task": task,
        "active": active.unwrap_or_else(|| previous["active"].as_bool().unwrap_or(false)),
        "event": name,
        "at": crate::engine::now(),
        "session_id": event["session_id"].as_str().or(previous["session_id"].as_str()),
        "turn_id": event["turn_id"].as_str(),
        "transcript_path": event["transcript_path"].as_str().or(previous["transcript_path"].as_str()),
    });
    if std::fs::create_dir_all(&dir).is_ok() {
        let tmp = dir.join(format!(".{task}.json.{}", std::process::id()));
        if std::fs::write(&tmp, record.to_string()).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
    0
}
pub fn run(name: &'static str) -> i32 {
    if name == "qream" && std::env::args().nth(1).as_deref() == Some("hook") {
        if std::env::args().nth(2).as_deref() == Some("--help") {
            println!("Usage: qream hook   (a harness's turn hook; reads the hook's JSON on stdin)");
            return 0;
        }
        return hook();
    }
    match execute(name) {
        Ok(code) => code,
        Err(error) => {
            // Exit 1 on a refusal the operator can act on, 2 on a fault or an unbuilt verb.
            let (code, exit) = match &error {
                Error::Arguments(_) => ("arguments".to_owned(), 2),
                Error::Refused(_) => ("refused".to_owned(), 1),
                Error::Engine(e) => {
                    let code = e.code().to_owned();
                    let exit = if matches!(
                        code.as_str(),
                        "refused"
                            | "busy"
                            | "invalid"
                            | "read_only"
                            | "held"
                            | "stopped"
                            | "live_process"
                            | "not_owned"
                            | "not_found"
                    ) {
                        1
                    } else {
                        2
                    };
                    (code, exit)
                }
                _ => ("fault".to_owned(), 2),
            };
            let _ = print_json(&json!({"error":{"code":code,"message":error.to_string()}}));
            exit
        }
    }
}

/// Every verb the command tree carries, as `verb` or `group verb`.
pub fn verbs() -> Vec<String> {
    let mut verbs = Vec::new();
    for sub in command("qream").get_subcommands() {
        let subs: Vec<_> = sub.get_subcommands().collect();
        if subs.is_empty() {
            verbs.push(sub.get_name().to_owned());
        } else {
            verbs.extend(
                subs.iter()
                    .map(|v| format!("{} {}", sub.get_name(), v.get_name())),
            );
        }
    }
    verbs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_contract_verb_is_in_the_tree() {
        command("qream").debug_assert();
        let verbs = verbs();
        for verb in [
            "gui",
            "daemon start",
            "daemon stop",
            "daemon status",
            "campaigns",
            "campaign up",
            "campaign down",
            "campaign trigger",
            "control",
            "setup new",
            "setup show",
            "setup check",
            "setup add-directive",
            "setup move",
            "setup manifest",
            "setup confirm",
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
            "row check",
            "gapfill",
            "profile",
            "capacity",
            "quota",
            "lint",
            "fence",
            "recover",
            "refresh",
            "leftovers",
            "events",
            "harness up",
            "harness status",
            "chief",
            "controller start",
            "controller trigger",
            "controller status",
            "release-record",
            "replay record",
            "replay list",
            "replay summary",
            "faults add",
            "faults amend",
            "faults list",
            "notify",
        ] {
            assert!(verbs.iter().any(|v| v == verb), "{verb}");
        }
    }

    #[test]
    fn term_words_become_one_request() {
        let words: Vec<String> = ["pane", "text", "--pane", "w1:p1", "--lines", "20"]
            .map(String::from)
            .to_vec();
        let (cmd, args) = term_request(&words).expect("request");
        assert_eq!(cmd, "term.pane.text");
        assert_eq!(args, json!({"pane":"w1:p1","lines":20}));
    }
}
