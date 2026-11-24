//! Setup creates and edits a commissioned pack. It never generates directive prose or launches a harness.
use crate::{
    Result,
    error::{io, mkdir, read, refuse, write},
    folder::{self, Campaign, Close, Directive, Manifest, ManifestTask},
    rows::{Assignment, AssignmentSink, Row, RowSource},
    rules::{self, Findings, Level, LintOptions},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Scope {
    pub rows: Vec<String>,
    pub groups: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Route {
    pub kind: String,
    pub model: String,
    pub effort: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Refusal {
    pub rule: String,
    pub file: PathBuf,
    pub line: usize,
    pub row: Option<String>,
    pub words: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Check {
    pub ok: bool,
    pub refusals: Vec<Refusal>,
    pub findings: Findings,
}
fn valid(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
}
fn selected(scope: &Scope, source: &dyn RowSource) -> Result<BTreeMap<String, Row>> {
    let mut ids: BTreeSet<_> = scope.rows.iter().cloned().collect();
    for group in &scope.groups {
        let members = source.members(group)?;
        if members.is_empty() {
            return Err(refuse("R5", group, group, "picked group has no members"));
        }
        ids.extend(members);
    }
    if ids.is_empty() {
        return Err(refuse(
            "R5",
            "campaign",
            "campaign row selection",
            "pick at least one row or group",
        ));
    }
    let mut rows = BTreeMap::new();
    for id in ids {
        let row = source
            .get(&id)?
            .ok_or_else(|| refuse("R1", &id, &id, "row does not exist in tracker"))?;
        if row.thin() {
            return Err(refuse(
                "R4",
                &id,
                &row.purpose,
                "purpose needs forty characters and definition of done must exist",
            ));
        }
        rows.insert(id, row);
    }
    Ok(rows)
}
fn cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', "<br>")
}
fn reserve(path: &Path, text: &str) -> Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| io(path, e))?;
    f.write_all(text.as_bytes()).map_err(|e| io(path, e))
}
/// Only group text, row selection and fixed skeleton files are emitted; the planning agent writes all directives.
pub fn new(root: &Path, name: &str, scope: &Scope, source: &dyn RowSource) -> Result<Campaign> {
    if !valid(name) {
        return Err(refuse(
            "R5",
            name,
            name,
            "campaign needs a safe one-line name",
        ));
    }
    if root.symlink_metadata().is_ok() {
        return Err(refuse(
            "L20",
            name,
            name,
            "campaign folder already exists; preserve its pack",
        ));
    }
    let rows = selected(scope, source)?;
    let mut groups: BTreeSet<_> = scope.groups.iter().cloned().collect();
    groups.extend(
        rows.values()
            .filter(|r| !r.outcome_id.is_empty())
            .map(|r| r.outcome_id.clone()),
    );
    let mut outcomes = Vec::new();
    for id in groups {
        let g = source
            .group(&id)?
            .ok_or_else(|| refuse("R5", &id, &id, "group text absent from tracker"))?;
        if g.outcome.trim().is_empty() {
            return Err(refuse("R5", &id, &id, "group has no recorded outcome"));
        }
        outcomes.push(g);
    }
    if outcomes.is_empty() {
        return Err(refuse(
            "R5",
            name,
            name,
            "picked rows need recorded outcome groups",
        ));
    }
    let group_ids = outcomes
        .iter()
        .map(|g| g.id.clone())
        .collect::<Vec<_>>()
        .join(", ");
    let mut charter = format!("# {group_ids} — {name}\n\n## Outcome\n");
    for g in &outcomes {
        charter.push_str(&g.outcome);
        charter.push('\n');
    }
    charter.push_str("\n## Result lines\n| N | Result | Standing | Group | Rows |\n| --- | --- | --- | --- | --- |\n");
    for (n, g) in outcomes.iter().enumerate() {
        let ids = rows
            .values()
            .filter(|r| r.outcome_id == g.id)
            .map(|r| r.id.clone())
            .collect::<Vec<_>>()
            .join(", ");
        charter.push_str(&format!(
            "| {} | {} | {} | {} | {} |\n",
            n + 1,
            cell(&g.outcome),
            cell(&g.standing),
            g.id,
            ids
        ));
    }
    charter.push_str("\n## Proof journey\n");
    for g in &outcomes {
        if !g.proof.trim().is_empty() {
            charter.push_str(&g.proof);
            charter.push('\n');
        }
    }
    charter.push_str("\n## Measures\nDefinitions of done of the picked rows, proven by a non-author.\n\n## Not-done-if\nA picked row's definition of done remains unproven.\n");
    // An empty `Rows:` or `Groups:` line is left out: the skill's outcome check reads the next line
    // as its list (an empty `Groups:` made `Deferred:` a group).
    let mut picks = String::new();
    for (label, list) in [("Rows", &scope.rows), ("Groups", &scope.groups)] {
        if !list.is_empty() {
            picks.push_str(&format!("{label}: {}\n", list.join(", ")));
        }
    }
    let order = format!(
        "# Deployment order — {name}\n\n{picks}Deferred:\n\n## Close\nClose must be declared by confirm before Trigger.\n"
    );
    if let Some(parent) = root.parent() {
        mkdir(parent)?;
    }
    std::fs::create_dir(root).map_err(|e| io(root, e))?;
    for file in folder::FILES {
        let text = match file {
            "OUTCOME.md" => charter.clone(),
            "DEPLOYMENT_ORDER.md" => order.clone(),
            "CLOSE.json" => "null\n".into(),
            "INTERFACE_PATHS.txt" => "# Graphical interface paths\n".into(),
            "PRE_FLIGHT.md" => {
                "# Preflight\n\nBLOCKER: author has not completed ten checks and signatures.\n"
                    .into()
            }
            _ => format!("# {}\n", file.trim_end_matches(".md")),
        };
        reserve(&root.join(file), &text)?;
    }
    for dir in folder::DIRS {
        mkdir(&root.join(dir))?;
    }
    mkdir(&root.join("wave1"))?;
    crate::ledger::Journal::new(root).initialize()?;
    write(&root.join("state/admission.jsonl"), "")?;
    let manifest = Manifest {
        name: format!("{name}-w1"),
        parallel: 0,
        campaign: name.into(),
        tasks: Vec::new(),
    };
    write(
        &root.join("wave1.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    Campaign::open(root)
}
pub fn show(root: &Path) -> Result<Campaign> {
    Campaign::open(root)
}
pub fn check(root: &Path, source: &dyn RowSource) -> Result<Check> {
    let mut opts = LintOptions::new(source)?;
    opts.campaign_root = Some(root.into());
    let findings = rules::lint(root, &opts)?;
    let refusals = findings
        .reports
        .iter()
        .flat_map(|r| {
            r.findings
                .iter()
                .filter(|f| f.level == Level::Blocking)
                .map(|f| Refusal {
                    rule: f.rule.clone(),
                    file: r.path.clone(),
                    line: f.line,
                    row: f.row.clone(),
                    words: f.message.clone(),
                })
        })
        .collect::<Vec<_>>();
    Ok(Check {
        ok: refusals.is_empty(),
        refusals,
        findings,
    })
}
fn within_close(root: &Path, wave: u32) -> Result<()> {
    if wave == 0 {
        return Err(refuse(
            "L20",
            "wave",
            "campaign wave selection",
            "wave must be positive",
        ));
    }
    let campaign = Campaign::open(root)?;
    if campaign.close.is_some_and(|c| wave > c.last_wave) {
        return Err(refuse(
            "R5",
            "wave",
            "declared campaign close",
            "wave exceeds the declared close",
        ));
    }
    Ok(())
}
fn used(root: &Path, task: &str) -> Result<bool> {
    let c = Campaign::open(root)?;
    if c.waves
        .iter()
        .any(|w| w.directives.iter().any(|d| d.id == task))
        || root.join("artifacts").join(task).exists()
        || root.join("work").join(task).exists()
    {
        return Ok(true);
    }
    let ledger = root.join("state/task_ids.json");
    let ids: Vec<String> = if ledger.exists() {
        folder::json(&ledger)?
    } else {
        Vec::new()
    };
    Ok(ids.iter().any(|id| id == task))
}
fn remember(root: &Path, task: &str) -> Result<()> {
    let ledger = root.join("state/task_ids.json");
    let mut ids: Vec<String> = if ledger.exists() {
        folder::json(&ledger)?
    } else {
        Vec::new()
    };
    if !ids.iter().any(|id| id == task) {
        ids.push(task.into());
    }
    write(&ledger, serde_json::to_vec_pretty(&ids)?)
}
fn campaign_name(c: &Campaign) -> String {
    c.close
        .as_ref()
        .map(|c| c.campaign.clone())
        .or_else(|| {
            c.waves
                .iter()
                .find_map(|w| w.manifest.as_ref().map(|m| m.campaign.clone()))
        })
        .unwrap_or_else(|| {
            c.root
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into()
        })
}
fn preserve(root: &Path, path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let dir = root.join("artifacts/pack_versions");
    mkdir(&dir)?;
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    for n in 1u64.. {
        let p = dir.join(format!("{name}.v{n}"));
        if !p.exists() {
            std::fs::copy(path, &p).map_err(|e| io(&p, e))?;
            return Ok(());
        }
    }
    Ok(())
}
pub fn add_directive(
    root: &Path,
    wave: u32,
    task: &str,
    file: &Path,
    source: &dyn RowSource,
    sink: &dyn AssignmentSink,
) -> Result<Directive> {
    if !valid(task)
        || !task.starts_with('T')
        || !task.chars().nth(1).is_some_and(|c| c.is_ascii_digit())
    {
        return Err(refuse("L20", task, task, "invalid task id"));
    }
    within_close(root, wave)?;
    if used(root, task)? {
        return Err(refuse(
            "L20",
            task,
            task,
            "campaign-wide task id already used",
        ));
    }
    let name = file
        .file_name()
        .ok_or_else(|| refuse("L20", task, task, "directive filename missing"))?;
    let target = root.join(format!("wave{wave}")).join(name);
    let d = Directive::parse(&target, read(file)?)?;
    if d.id != task {
        return Err(refuse("L20", task, task, "filename and task id differ"));
    }
    let mut opts = LintOptions::new(source)?;
    opts.campaign_root = Some(root.into());
    opts.coverage = false;
    let findings = rules::lint(file, &opts)?;
    if let Some(f) = findings
        .reports
        .iter()
        .flat_map(|r| &r.findings)
        .find(|f| f.level == Level::Blocking)
    {
        return Err(refuse(
            &f.rule,
            f.row.as_deref().unwrap_or(task),
            &d.title,
            &f.message,
        ));
    }
    let c = Campaign::open(root)?;
    let mut picked: BTreeSet<_> = c.deployment.rows.iter().cloned().collect();
    for group in &c.deployment.groups {
        picked.extend(source.members(group)?);
    }
    for id in d.served().keys() {
        if !picked.contains(id) {
            return Err(refuse("R5", id, id, "served row was not picked"));
        }
    }
    // Check ownership before assigning or writing the directive.
    if let Some(w) = c.waves.iter().find(|w| w.number == wave) {
        let proposed = rules::paths(d.body("Writable scope"))?;
        for old in &w.directives {
            for a in rules::paths(old.body("Writable scope"))? {
                for b in &proposed {
                    if overlap_paths(&a, b) {
                        return Err(refuse(
                            "L25",
                            task,
                            &d.title,
                            "same-wave writable scopes overlap",
                        ));
                    }
                }
            }
        }
    }
    mkdir(target.parent().unwrap_or(root))?;
    mkdir(&root.join("work").join(task).join("candidate"))?;
    mkdir(&root.join("artifacts").join(task))?;
    remember(root, task)?;
    reserve(&target, &d.text)?;
    let name = campaign_name(&c);
    for (id, part) in d.served() {
        sink.assign(&Assignment {
            row: id,
            campaign: name.clone(),
            task: task.into(),
            wave,
            directive: target.clone(),
            part,
        })?;
    }
    Ok(d)
}
fn overlap_paths(a: &str, b: &str) -> bool {
    let a = Path::new(a);
    let b = Path::new(b);
    a == b || a.starts_with(b) || b.starts_with(a)
}
pub fn move_task(root: &Path, task: &str, wave: u32) -> Result<Directive> {
    within_close(root, wave)?;
    let c = Campaign::open(root)?;
    let matches = c
        .waves
        .iter()
        .flat_map(|w| &w.directives)
        .filter(|d| d.id == task)
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        return Err(refuse("L20", task, task, "task must occur exactly once"));
    }
    let d = matches[0];
    let target = root
        .join(format!("wave{wave}"))
        .join(d.path.file_name().unwrap_or_default());
    if target == d.path {
        return Ok(d.clone());
    }
    if target.exists() {
        return Err(refuse(
            "L20",
            task,
            &d.title,
            "destination task id already used",
        ));
    }
    if let Some(w) = c.waves.iter().find(|w| w.number == wave) {
        for old in &w.directives {
            for a in rules::paths(old.body("Writable scope"))? {
                for b in rules::paths(d.body("Writable scope"))? {
                    if overlap_paths(&a, &b) {
                        return Err(refuse(
                            "L25",
                            task,
                            &d.title,
                            "destination wave writable scopes overlap",
                        ));
                    }
                }
            }
        }
    }
    preserve(root, &d.path)?;
    mkdir(target.parent().unwrap_or(root))?;
    std::fs::rename(&d.path, &target).map_err(|e| io(&target, e))?;
    for w in &c.waves {
        if let Some(m) = &w.manifest
            && m.tasks.iter().any(|t| t.id == task)
        {
            let p = root.join(format!("wave{}.json", w.number));
            preserve(root, &p)?;
            let mut m = m.clone();
            m.tasks.retain(|t| t.id != task);
            m.parallel = m.tasks.len();
            write(&p, serde_json::to_vec_pretty(&m)?)?;
        }
    }
    remember(root, task)?;
    Directive::open(&target)
}
/// Routes retain harness model/effort flags. No bypass of native approval, sandbox, account or login is emitted.
pub fn manifest(
    root: &Path,
    wave: u32,
    default: &Route,
    tasks: &BTreeMap<String, Route>,
) -> Result<Manifest> {
    within_close(root, wave)?;
    let c = Campaign::open(root)?;
    let w = c.waves.iter().find(|w| w.number == wave).ok_or_else(|| {
        refuse(
            "L20",
            "wave",
            "campaign wave manifest",
            "wave directory missing",
        )
    })?;
    if w.directives.is_empty() {
        return Err(refuse(
            "L20",
            "wave",
            "campaign wave manifest",
            "wave requires at least one directive",
        ));
    }
    let ids: BTreeSet<_> = w.directives.iter().map(|d| d.id.as_str()).collect();
    if let Some(id) = tasks.keys().find(|id| !ids.contains(id.as_str())) {
        return Err(refuse("L23", id, id, "per-task route names an absent task"));
    }
    let mut entries = Vec::new();
    for d in &w.directives {
        let route = tasks.get(&d.id).unwrap_or(default);
        let cwd = c.root.join("work").join(&d.id).join("candidate");
        mkdir(&cwd)?;
        // A harness worker gets its full unattended profile (permissions, sandbox, folder trust);
        // the bare route stopped Setup-built workers at the harness's permission prompt (walk fault 3).
        let args = match route.kind.as_str() {
            "claude" | "codex" | "opencode" => {
                crate::profile::args_for_cwd(&route.kind, &route.model, &route.effort, &cwd)?
            }
            _ => rules::route_args(&route.kind, &route.model, &route.effort)?,
        };
        entries.push(ManifestTask {
            id: d.id.clone(),
            cwd,
            directive: d.path.clone(),
            kind: route.kind.clone(),
            args,
        });
    }
    let name = campaign_name(&c);
    let m = Manifest {
        name: format!("{name}-w{wave}"),
        campaign: name,
        parallel: entries.len(),
        tasks: entries,
    };
    let path = root.join(format!("wave{wave}.json"));
    preserve(root, &path)?;
    write(&path, serde_json::to_vec_pretty(&m)?)?;
    Ok(m)
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Card {
    pub id: String,
    pub title: String,
    pub wave: u32,
    pub directive: PathBuf,
    pub rows: BTreeMap<String, bool>,
    pub kind: Option<String>,
    pub args: Vec<String>,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Draft {
    pub scope: Scope,
    pub waves: Vec<u32>,
    pub cards: Vec<Card>,
}
pub fn draft(root: &Path) -> Result<Draft> {
    let c = Campaign::open(root)?;
    let cards = c
        .waves
        .iter()
        .flat_map(|w| {
            w.directives.iter().map(|d| {
                let route = w
                    .manifest
                    .as_ref()
                    .and_then(|m| m.tasks.iter().find(|t| t.id == d.id));
                Card {
                    id: d.id.clone(),
                    title: d.title.clone(),
                    wave: w.number,
                    directive: d.path.clone(),
                    rows: d.served(),
                    kind: route.map(|t| t.kind.clone()),
                    args: route.map(|t| t.args.clone()).unwrap_or_default(),
                }
            })
        })
        .collect();
    Ok(Draft {
        scope: Scope {
            rows: c.deployment.rows,
            groups: c.deployment.groups,
        },
        waves: c.waves.iter().map(|w| w.number).collect(),
        cards,
    })
}
pub fn confirm(
    root: &Path,
    close: Option<Close>,
    source: &dyn RowSource,
) -> Result<serde_json::Value> {
    if close.is_none() {
        crate::close::require_declared(root).map_err(|error| match error {
            crate::Error::Denied(reason) => refuse(
                "R5",
                "campaign",
                "declared campaign close",
                &format!("declare last wave and expected close before Confirm: {reason}"),
            ),
            other => other,
        })?;
    }
    let c = Campaign::open(root)?;
    let close = close.or(c.close).ok_or_else(|| {
        refuse(
            "R5",
            "campaign",
            "declared campaign close",
            "declare last wave and expected close before Confirm",
        )
    })?;
    crate::close::validate_declaration(close.last_wave, &close.expected_close)?;
    if close.last_wave == 0
        || close.expected_close.trim().is_empty()
        || close.declared_at.trim().is_empty()
        || close.by.trim().is_empty()
        || close.rule.trim().is_empty()
    {
        return Err(refuse(
            "R5",
            "campaign",
            "declared campaign close",
            "close fields must be complete",
        ));
    }
    if c.waves.iter().any(|w| w.number > close.last_wave) {
        return Err(refuse(
            "R5",
            "campaign",
            "declared campaign close",
            "prepared wave exceeds declared last wave",
        ));
    }
    let checked = check(root, source)?;
    if !checked.ok {
        return Ok(serde_json::json!({"ok":false,"refusals":checked.refusals}));
    }
    let preflight = read(&root.join("PRE_FLIGHT.md"))?;
    if preflight.contains("BLOCKER") {
        return Err(refuse(
            "R5",
            "campaign",
            "author preflight before campaign trigger",
            "PRE_FLIGHT.md has a BLOCKER",
        ));
    }
    if c.waves
        .iter()
        .any(|w| w.directives.is_empty() || w.manifest.is_none())
    {
        return Err(refuse(
            "L20",
            "campaign",
            "prepared campaign wave manifests",
            "every wave needs directives and a manifest",
        ));
    }
    let p = root.join("CLOSE.json");
    preserve(root, &p)?;
    write(&p, serde_json::to_vec_pretty(&close)?)?;
    let order = root.join("DEPLOYMENT_ORDER.md");
    preserve(root, &order)?;
    let mut text = read(&order)?;
    if let Some(pos) = text.find("\n## Close") {
        text.truncate(pos);
    }
    text.push_str(&format!(
        "\n\n## Close\nLast wave: {}\nExpected close: {}\nDeclared at: {}\nBy: {}\nRule: {}\n",
        close.last_wave, close.expected_close, close.declared_at, close.by, close.rule
    ));
    write(&order, text)?;
    Ok(serde_json::json!({"ok":true,"state":"prepared","close":close}))
}

/// The picked scope, in pack order, or the recorded assignments when no picks were written.
/// Store lookups stay in the library; a window receives plain words and never reads files.
pub fn campaign_scope(
    root: &Path,
    campaign: &str,
    store: &tracker_store::Store,
) -> Result<serde_json::Value> {
    let order = root.join("DEPLOYMENT_ORDER.md");
    let mut scope = match std::fs::read_to_string(&order) {
        Ok(text) => {
            let deployment = folder::Deployment::parse(&text);
            Scope {
                rows: deployment.rows,
                groups: deployment.groups,
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Scope::default(),
        Err(error) => return Err(io(&order, error)),
    };
    if scope.rows.is_empty() && scope.groups.is_empty() {
        for assignment in store.assignments(campaign)?.into_iter().rev() {
            if let Some(row) = RowSource::get(store, &assignment.row_id)?
                && !row.outcome_id.is_empty()
            {
                scope.groups.push(row.outcome_id);
            }
            scope.rows.push(assignment.row_id);
        }
    }
    let mut rows = Vec::new();
    let mut groups = Vec::new();
    let mut seen = BTreeSet::new();
    for id in scope.rows {
        if !seen.insert(id.clone()) {
            continue;
        }
        let row = RowSource::get(store, &id)?
            .ok_or_else(|| crate::Error::NotFound(format!("tracker row {id}")))?;
        let words = crate::rows::norm(&row.purpose);
        rows.push(serde_json::json!({"id":id,"kind":"row","status":row.status,"purpose":words,"words":words}));
    }
    seen.clear();
    for id in scope.groups {
        if !seen.insert(id.clone()) {
            continue;
        }
        let row = store.group(&id)?;
        let text = |key: &str| {
            row.get(key)
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
        };
        let words = crate::rows::norm(text("outcome"));
        let status = ["status", "work_state", "standing"]
            .into_iter()
            .map(text)
            .find(|s| !s.is_empty())
            .unwrap_or("");
        groups.push(serde_json::json!({"id":id,"kind":"group","status":status,"standing":text("standing"),"outcome":words,"words":words}));
    }
    let items: Vec<_> = rows.iter().chain(&groups).cloned().collect();
    let message = if items.is_empty() {
        "This campaign has no tracker rows or outcome groups."
    } else {
        ""
    };
    Ok(
        serde_json::json!({"campaign":campaign,"rows":rows,"groups":groups,"items":items,"message":message}),
    )
}
