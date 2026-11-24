//! One rule table, one evaluator for file, wave, campaign and manifest callers.
use crate::{
    Error, Result,
    error::read,
    folder::{self, Directive, Manifest},
    rows::{RowSource, norm},
};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Level {
    Blocking,
    Note,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    pub id: String,
    pub severity: Level,
    pub text: String,
    pub check: String,
    #[serde(default)]
    pub pattern: Option<String>,
}
#[derive(Debug, Clone, Deserialize)]
pub struct RuleTable {
    pub version: u32,
    pub rules: Vec<Rule>,
}
impl RuleTable {
    pub fn standard() -> Result<Self> {
        Ok(toml::from_str(include_str!("../rules/table.toml"))?)
    }
    pub fn open(path: &Path) -> Result<Self> {
        Ok(toml::from_str(&read(path)?)?)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    pub rule: String,
    pub level: Level,
    pub line: usize,
    pub text: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub row: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub path: PathBuf,
    pub findings: Vec<Finding>,
    pub blocking: usize,
    pub note: usize,
}
impl Report {
    fn new(path: &Path) -> Self {
        Self {
            path: path.into(),
            findings: Vec::new(),
            blocking: 0,
            note: 0,
        }
    }
    fn add(
        &mut self,
        rule: &Rule,
        level: Level,
        line: usize,
        text: &str,
        row: Option<(&str, &str)>,
        reason: &str,
    ) {
        let (id, purpose) =
            row.unwrap_or(("campaign", "campaign preparation and directive validation"));
        let words = purpose
            .split_whitespace()
            .take(6)
            .collect::<Vec<_>>()
            .join(" ");
        self.findings.push(Finding {
            rule: rule.id.clone(),
            level,
            line,
            text: text.trim().chars().take(160).collect(),
            message: format!("{} refuses {} ({}): {}", rule.id, id, words, reason),
            row: row.map(|(id, _)| id.to_owned()),
        });
    }
    fn finish(&mut self) {
        self.findings.sort_by(|a, b| {
            ((a.level != Level::Blocking), &a.rule, a.line).cmp(&(
                (b.level != Level::Blocking),
                &b.rule,
                b.line,
            ))
        });
        self.blocking = self
            .findings
            .iter()
            .filter(|f| f.level == Level::Blocking)
            .count();
        self.note = self.findings.len() - self.blocking;
    }
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Findings {
    pub blocking: usize,
    pub note: usize,
    pub reports: Vec<Report>,
}
impl Findings {
    pub fn finish(&mut self) {
        for r in &mut self.reports {
            r.finish();
        }
        self.blocking = self.reports.iter().map(|r| r.blocking).sum();
        self.note = self.reports.iter().map(|r| r.note).sum();
    }
}
pub struct LintOptions<'a> {
    pub rows: &'a dyn RowSource,
    pub campaign_root: Option<PathBuf>,
    pub table: RuleTable,
    pub row_checks: bool,
    pub coverage: bool,
    pub charter: Option<PathBuf>,
    pub lanes: Option<PathBuf>,
}
impl<'a> LintOptions<'a> {
    pub fn new(rows: &'a dyn RowSource) -> Result<Self> {
        Ok(Self {
            rows,
            campaign_root: None,
            table: RuleTable::standard()?,
            row_checks: true,
            coverage: true,
            charter: None,
            lanes: None,
        })
    }
    /// Extra pattern rules in the skill's `lint_rules.json` shape replace same-id rules.
    pub fn extra_rules(&mut self, extra: &Path) -> Result<()> {
        let value: serde_json::Value = crate::folder::json(extra)?;
        let rules = value.as_array().ok_or_else(|| Error::Parse {
            path: extra.to_owned(),
            line: 0,
            message: "extra rules must be an array".into(),
        })?;
        for rule in rules {
            let id = rule["id"].as_str().unwrap_or("");
            let pattern = rule["regex"].as_str().ok_or_else(|| Error::Parse {
                path: extra.to_owned(),
                line: 0,
                message: "extra rule needs regex".into(),
            })?;
            Regex::new(pattern)?;
            self.table.rules.retain(|r| r.id != id);
            self.table.rules.push(Rule {
                id: id.into(),
                severity: if rule["level"] == "NOTE" {
                    Level::Note
                } else {
                    Level::Blocking
                },
                text: rule["message"].as_str().unwrap_or("").into(),
                check: "pattern".into(),
                pattern: Some(pattern.into()),
            });
        }
        Ok(())
    }
}
fn re(pattern: &str) -> Result<Regex> {
    Ok(Regex::new(pattern)?)
}
fn numbered(body: &str) -> Vec<(usize, &str)> {
    body.lines()
        .enumerate()
        .filter(|(_, l)| {
            l.split_once(". ")
                .is_some_and(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
        })
        .map(|(n, l)| (n + 1, l))
        .collect()
}
pub fn paths(body: &str) -> Result<Vec<String>> {
    Ok(
        re(r#"(?:/home/[^\s`,;\)\"']+|(?:work|reports|artifacts|design|spec)/[^\s`,;\)\"']+)"#)?
            .find_iter(body)
            .map(|m| {
                m.as_str()
                    .trim_end_matches('.')
                    .trim_end_matches('/')
                    .to_owned()
            })
            .collect(),
    )
}
fn fixed(line: &str) -> bool {
    line.contains("Never run a command that raises a confirmation")
        || line.contains("(1) Check a premise before acting on it")
        || (line.starts_with("If ") && line.contains("/STOP exists, stop and go idle."))
}
fn prohibition(line: &str) -> bool {
    let s = line.to_lowercase();
    [
        "no ",
        "never",
        "not ",
        "forbid",
        "refuse",
        "must not",
        "do not",
        "don't",
        "read-only",
        "excluded",
        "without",
        "except",
        "nothing",
    ]
    .iter()
    .any(|p| s.contains(p))
}
fn logical(lines: &[&str], i: usize) -> String {
    let mut start = i;
    while start > 0
        && !lines[start - 1].trim().is_empty()
        && !lines[start].trim_start().starts_with(['-', '*', '#'])
        && !lines[start]
            .split_once(". ")
            .is_some_and(|(n, _)| n.chars().all(|c| c.is_ascii_digit()))
    {
        start -= 1;
    }
    lines[start..=i].join(" ")
}
fn resolved(root: &Path, path: &str) -> PathBuf {
    let p = Path::new(path);
    if p.is_absolute() {
        p.to_owned()
    } else {
        root.join(p)
    }
}
fn prefix(a: &Path, b: &Path) -> bool {
    a == b || b.starts_with(a)
}
pub fn pattern_path(pattern: &str, path: &str) -> Result<bool> {
    // Shell-style globs used by INTERFACE_PATHS and writable scopes. * crosses directories, like skill fnmatch.
    let mut out = String::from("^");
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' => out.push_str(".*"),
            '?' => out.push('.'),
            '{' => out.push_str("(?:"),
            '}' => out.push(')'),
            ',' => out.push('|'),
            '[' => {
                out.push('[');
                for c in chars.by_ref() {
                    out.push(c);
                    if c == ']' {
                        break;
                    }
                }
            }
            _ => out.push_str(&regex::escape(&c.to_string())),
        }
    }
    out.push('$');
    let r = re(&out)?;
    Ok(r.is_match(path)
        || r.is_match(&format!("{path}/x"))
        || (!pattern.contains(['*', '?', '[', '{']) && path.starts_with(&format!("{pattern}/"))))
}
fn exclusions(d: &Directive, rule: &Rule) -> Result<Vec<(usize, String)>> {
    let quotes = re(r#""[^"]*"|'[^']*'|`[^`]*`"#)?;
    let rx = re(
        r"(?i)^\s*(boundaries|boundary)\s*[(:]|\bnot in (the )?scope\b|\bout of (the )?scope\b|\bnot (in|part of) this campaign\b|\bno [a-z ]{0,30}playback\b",
    )?;
    let mut rows = false;
    let mut out = Vec::new();
    for (n, l) in d.text.lines().enumerate() {
        if l.starts_with("## ") {
            rows = l.trim() == "## Rows";
        }
        if fixed(l) || (rows && rule.id == "R3") {
            continue;
        }
        if rx.is_match(&quotes.replace_all(l, "")) {
            out.push((n + 1, l.into()));
        }
    }
    Ok(out)
}
fn root_of(path: &Path, opts: &LintOptions<'_>) -> Result<PathBuf> {
    if let Some(root) = &opts.campaign_root {
        return Ok(root.clone());
    }
    let abs = std::path::absolute(path).map_err(|e| crate::error::io(path, e))?;
    let mut p = if abs.is_file() {
        abs.parent().unwrap_or(&abs).to_owned()
    } else {
        abs
    };
    if p.file_name()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.starts_with("wave"))
    {
        p.pop();
    }
    Ok(p)
}
fn lint_directive(d: &Directive, root: &Path, opts: &LintOptions<'_>) -> Result<Report> {
    let namespace = std::env::var_os("QREAM_NAMESPACE_ROOT")
        .map(PathBuf::from)
        .map(Ok)
        .unwrap_or_else(crate::daemon::operator_home)?;
    let mut report = Report::new(&d.path);
    let lines: Vec<_> = d.text.lines().collect();
    let served = d.served();
    let blocks = d.row_fields();
    let mut store = BTreeMap::new();
    if opts.row_checks {
        for id in served.keys() {
            match opts.rows.get(id) {
                Ok(row) => {
                    store.insert(id.clone(), row);
                }
                Err(e) => {
                    return Err(e);
                }
            }
        }
    }
    let charter = if root.join("OUTCOME.md").exists() {
        read(&root.join("OUTCOME.md"))?
    } else {
        String::new()
    };
    let historical = root
        .join("artifacts")
        .join(&d.id)
        .join("freeze.json")
        .exists();
    let outcome =
        re(r"(?m)^Outcome:\s*([A-Z][A-Z0-9-]+),\s*result lines?\s+([0-9a-zA-Z, and–-]+?)\s*[:(]")?;
    let writes = paths(d.body("Writable scope"))?;
    for rule in &opts.table.rules {
        let mut add = |lv: Level, n: usize, l: &str, row: Option<(&str, &str)>, msg: &str| {
            report.add(rule, lv, n, l, row, msg)
        };
        match rule.check.as_str() {
            "sections" => {
                for name in folder::SECTIONS {
                    if d.section(name).is_none() {
                        add(
                            rule.severity,
                            0,
                            "",
                            None,
                            &format!("missing section: {name}"),
                        );
                    }
                }
            }
            "acceptance_count" => {
                if numbered(d.body("Acceptance checklist")).len() < 3 {
                    add(
                        rule.severity,
                        0,
                        "",
                        None,
                        "acceptance checklist needs at least three numbered checks",
                    );
                }
            }
            "acceptance_observable" => {
                for (n, l) in numbered(d.body("Acceptance checklist")) {
                    if !l.contains('`')
                        && ![
                            "prints", "exits", "equals", "lists", "shows", "=", "≥", "≤", "->", "→",
                        ]
                        .iter()
                        .any(|w| l.contains(w))
                    {
                        add(
                            rule.severity,
                            d.section("Acceptance checklist").map_or(0, |s| s.line) + n,
                            l,
                            None,
                            "checklist item has no command or observable",
                        );
                    }
                }
            }
            "scope" => {
                if writes.is_empty() {
                    add(rule.severity, 0, "", None, "writable scope names no path");
                }
            }
            "seal" => {
                let rx = re(r"seal\.py\s+\S+\s+(T\d+(?:W\d+)?)\b")?;
                if !rx.captures_iter(&d.text).any(|c| c[1] == d.id) {
                    add(
                        rule.severity,
                        0,
                        "",
                        None,
                        "seal.py command missing or task differs from filename",
                    );
                }
            }
            "stop" => {
                if !d.text.to_lowercase().contains("stop and go idle") {
                    add(rule.severity, 0, "", None, "stop and go idle line missing");
                }
            }
            "report" => {
                let stem = d.path.file_stem().unwrap_or_default().to_string_lossy();
                if !d.text.contains(&format!("reports/{stem}.md")) {
                    add(rule.severity, 0, "", None, "report path not named");
                }
            }
            "size" => {
                if lines.len() > 250 || d.text.len() > 24 * 1024 {
                    add(
                        Level::Blocking,
                        0,
                        "",
                        None,
                        "directive exceeds 250 lines or 24 KiB",
                    );
                } else if lines.len() > 160 || d.text.len() > 12 * 1024 {
                    add(
                        Level::Note,
                        0,
                        "",
                        None,
                        "directive exceeds 160 lines or 12 KiB",
                    );
                }
            }
            "work_count" | "work_count_note" => {
                let n = numbered(d.body("Work items")).len();
                let limit = if rule.check == "work_count_note" {
                    8
                } else {
                    5
                };
                if n > limit && !(rule.id == "O9" && historical) {
                    add(
                        rule.severity,
                        0,
                        "",
                        None,
                        &format!("{n} work items: split into tasks of at most {limit}"),
                    );
                }
                if rule.id == "O9" && served.len() > 8 && !historical {
                    add(
                        Level::Note,
                        0,
                        "",
                        None,
                        "directive serves more than eight rows: consider splitting",
                    );
                }
            }
            "outcome" => {
                if !outcome.is_match(d.body("Orientation")) {
                    add(
                        rule.severity,
                        0,
                        "",
                        None,
                        "Outcome identifier and qualifying result line missing",
                    );
                }
            }
            "outcome_reference" => {
                let captures: Vec<_> = outcome.captures_iter(&d.text).collect();
                if captures.is_empty() {
                    add(
                        rule.severity,
                        0,
                        "",
                        None,
                        "Outcome identifier and qualifying result line missing",
                    );
                }
                for c in captures {
                    if !charter.is_empty() {
                        if !charter.contains(&c[1]) {
                            add(
                                rule.severity,
                                0,
                                "",
                                None,
                                "outcome identifier absent from OUTCOME.md",
                            );
                        }
                        let numbers = re(r"(\d+)\s*[-–]\s*(\d+)|(\d+[a-z]?|Z\d+)")?;
                        for n in numbers.captures_iter(&c[2]) {
                            let wanted = if let Some(single) = n.get(3) {
                                vec![single.as_str().to_owned()]
                            } else {
                                let a = n[1].parse::<u32>().unwrap_or(0);
                                let b = n[2].parse::<u32>().unwrap_or(0);
                                if b < a || b - a > 1000 {
                                    add(rule.severity, 0, "", None, "invalid result line range");
                                    Vec::new()
                                } else {
                                    (a..=b).map(|x| x.to_string()).collect()
                                }
                            };
                            for n in wanted {
                                if !re(&format!(r"(?m)^\|\s*{}\s*\|", regex::escape(&n)))?
                                    .is_match(&charter)
                                {
                                    add(
                                        rule.severity,
                                        0,
                                        "",
                                        None,
                                        &format!("result line {n} absent from OUTCOME.md"),
                                    );
                                }
                            }
                        }
                    }
                }
            }
            "served" => {
                if served.is_empty() {
                    add(
                        rule.severity,
                        0,
                        "",
                        None,
                        "no Serves rows line naming a tracker row",
                    );
                }
                for id in served.keys() {
                    if rule.id == "R1"
                        && opts.row_checks
                        && store.get(id).is_none_or(|r| r.is_none())
                    {
                        add(
                            rule.severity,
                            0,
                            "",
                            Some((id, id)),
                            "row does not exist in tracker",
                        );
                    }
                    if rule.id == "L28" && !blocks.contains_key(id) {
                        add(
                            rule.severity,
                            0,
                            "",
                            Some((id, id)),
                            "served row has no full block in Rows",
                        );
                    }
                }
            }
            "row_text" | "row_thin" => {
                if !opts.row_checks || (rule.id.starts_with('R') && historical) {
                    continue;
                }
                for id in served.keys() {
                    let Some(Some(row)) = store.get(id) else {
                        if rule.id == "L29" {
                            add(
                                Level::Blocking,
                                0,
                                "",
                                Some((id, id)),
                                "cannot read row from tracker",
                            );
                        }
                        continue;
                    };
                    let ctx = Some((id.as_str(), row.purpose.as_str()));
                    if rule.check == "row_thin" {
                        if row.thin() {
                            add(
                                rule.severity,
                                0,
                                "",
                                ctx,
                                "purpose needs forty characters and definition of done must exist; sharpen in tracker",
                            );
                        }
                    } else if let Some(fields) = blocks.get(id) {
                        for (label, value) in row.fields() {
                            if rule.id == "R2"
                                && !["Purpose", "Definition of done"].contains(&label)
                            {
                                continue;
                            }
                            let expected = if value.trim().is_empty() {
                                "none recorded"
                            } else {
                                value
                            };
                            if norm(fields.get(label).map(String::as_str).unwrap_or(""))
                                != norm(expected)
                            {
                                add(
                                    rule.severity,
                                    0,
                                    "",
                                    ctx,
                                    &format!("{label} differs from tracker; carry word for word"),
                                );
                            }
                        }
                    } else if rule.id == "R2" {
                        add(rule.severity, 0, "", ctx, "served row block missing");
                    }
                }
            }
            "exclusion" => {
                for (n, l) in exclusions(d, rule)? {
                    if rule.id == "L31" {
                        let sourced = blocks.iter().any(|(id, fields)| {
                            store.get(id).and_then(Option::as_ref).is_some_and(|r| {
                                r.fields().iter().any(|(label, value)| {
                                    fields.get(*label).is_some_and(|v| {
                                        norm(v)
                                            == norm(if value.trim().is_empty() {
                                                "none recorded"
                                            } else {
                                                value
                                            })
                                    }) && l.trim().starts_with(&format!("{label}:"))
                                })
                            })
                        });
                        if sourced {
                            continue;
                        }
                    }
                    add(
                        rule.severity,
                        n,
                        &l,
                        None,
                        "states what is not covered; state only what is in scope",
                    );
                }
            }
            "constitution" => {
                for p in &writes {
                    if (p.starts_with(
                        namespace
                            .join("00_constitution/")
                            .to_string_lossy()
                            .as_ref(),
                    ) || p.as_str()
                        == namespace.join("00_constitution").to_string_lossy().as_ref()
                        || p.starts_with("00_constitution"))
                        && !historical
                    {
                        add(
                            rule.severity,
                            0,
                            p,
                            None,
                            "writable scope touches the constitution",
                        );
                    }
                }
            }
            "write_scope" => {
                for (n, l) in lines.iter().enumerate() {
                    if !prohibition(&logical(&lines, n))
                        && re(r"(?i)sub-?agents?\b|spawn agents")?.is_match(l)
                    {
                        add(rule.severity, n + 1, l, None, "sub-agent instruction");
                    }
                }
                for p in &writes {
                    let p = resolved(root, p);
                    let permitted = [
                        root.join("work").join(&d.id),
                        root.join("artifacts").join(&d.id),
                        root.join("design"),
                        root.join("spec"),
                        root.join("reports").join(format!(
                            "{}.md",
                            d.path.file_stem().unwrap_or_default().to_string_lossy()
                        )),
                    ];
                    if !historical
                        && !permitted.iter().any(|r| prefix(r, &p))
                        && (p.starts_with(
                            namespace.join("06_memory/runs").to_string_lossy().as_ref(),
                        ) || p.starts_with(
                            namespace.join("04_memory/runs").to_string_lossy().as_ref(),
                        ))
                    {
                        add(
                            rule.severity,
                            0,
                            &p.display().to_string(),
                            None,
                            "writable scope outside task folders or in another campaign",
                        );
                    }
                    if !historical
                        && (p.starts_with(
                            namespace
                                .join("01_governance/skills")
                                .to_string_lossy()
                                .as_ref(),
                        ) || p.starts_with(
                            namespace.join(".codex/skills").to_string_lossy().as_ref(),
                        ) || p.starts_with(
                            namespace.join(".claude/skills").to_string_lossy().as_ref(),
                        ))
                    {
                        add(
                            rule.severity,
                            0,
                            &p.display().to_string(),
                            None,
                            "writable scope touches house rules",
                        );
                    }
                }
            }
            "installed" => {
                if re(r"qream-herdr/(releases|current)")?.is_match(d.body("Writable scope")) {
                    add(
                        Level::Note,
                        0,
                        "",
                        None,
                        "installed release in writable scope",
                    );
                }
            }
            "pattern" => {
                if let Some(pattern) = &rule.pattern {
                    let rx = re(pattern)?;
                    for (n, l) in lines.iter().enumerate() {
                        if rx.is_match(l) {
                            add(rule.severity, n + 1, l, None, &rule.text);
                        }
                    }
                }
            }
            "soliton" | "model_start" | "delete" | "dummy" | "opus" | "poll" | "haiku"
            | "live_session" => {
                let (pattern, absence) = match rule.check.as_str() {
                    "soliton" => ("soliton", false),
                    "model_start" => (
                        r"llm\.sh|llama-server|ollama|lm studio|vllm|qwen38|model server|--start-chief",
                        true,
                    ),
                    "delete" => (r"rm -rf|rm -r |rm -f |git clean|git reset --hard", false),
                    "dummy" => (r"placeholder|dummy data|lorem ipsum|\btodo\b", false),
                    "opus" => ("--model opus", false),
                    "poll" => (r"sleep.*(loop|while|every)|watch -n|poll every", false),
                    "haiku" => ("--model haiku", false),
                    _ => (
                        r"session (stop|delete)|workspace close|agent (prompt|send-keys)|pane close",
                        false,
                    ),
                };
                let rx = re(&format!("(?i){pattern}"))?;
                for (n, l) in lines.iter().enumerate() {
                    let context = logical(&lines, n);
                    if rule.check != "haiku" && prohibition(&context) {
                        continue;
                    }
                    if absence
                        && [
                            "pgrep",
                            "ps ",
                            "unchanged",
                            "before and after",
                            "assert",
                            "prints nothing",
                            "must not",
                        ]
                        .iter()
                        .any(|p| context.to_lowercase().contains(p))
                    {
                        continue;
                    }
                    if rule.check == "soliton" {
                        let own = charter
                            .lines()
                            .find(|s| s.starts_with("# "))
                            .is_some_and(|s| s.starts_with("# OC-12"));
                        let in_work = d
                            .sections
                            .iter()
                            .rev()
                            .find(|s| s.line < n + 1)
                            .is_some_and(|s| s.name == "Work items" || s.name == "Writable scope");
                        if own || !in_work {
                            continue;
                        }
                    }
                    let unquoted = if rule.check == "dummy" {
                        re(r"`[^`]*`")?.replace_all(l, "").into_owned()
                    } else {
                        l.to_string()
                    };
                    if rx.is_match(&unquoted) {
                        let mut lv = rule.severity;
                        if rule.check == "dummy" {
                            if !re(r"(?i)\b(text|data|string|render|display|show|ui|screen|label|interface)\b")?.is_match(&unquoted){continue;}
                            if re(r"(?i)\b(render|display|show)\b")?.is_match(&unquoted) {
                                lv = Level::Blocking;
                            }
                        }
                        if rule.check == "live_session"
                            && ![
                                "qream-master",
                                "qream-harness",
                                "qream-lorengine",
                                "qream-qroc",
                                "qream-zeno",
                                "qream-vasm",
                                "qream-fable",
                            ]
                            .iter()
                            .any(|s| l.contains(s))
                        {
                            continue;
                        }
                        add(lv, n + 1, l, None, &rule.text);
                    }
                    if rule.check == "delete" {
                        for c in re(r"session delete\s+(\S+)")?.captures_iter(l) {
                            let name = c[1].trim_matches(['`', ';', ',']);
                            if !name.starts_with('$')
                                && !re(r"^qream-(w\d+-t\d+|w\d+-accept|f\d+test|[a-z0-9-]*test)$")?
                                    .is_match(name)
                            {
                                add(
                                    rule.severity,
                                    n + 1,
                                    l,
                                    None,
                                    "deletion of non-private session",
                                );
                            }
                        }
                    }
                }
            }
            "source" => {
                if served.is_empty()
                    && !d
                        .body("Orientation")
                        .contains("Operator source by reference:")
                {
                    add(
                        Level::Note,
                        0,
                        "",
                        None,
                        "no operator source reference or served tracker row",
                    );
                }
            }
            _ => {}
        }
    }
    report.finish();
    Ok(report)
}
fn table_rule<'a>(opts: &'a LintOptions<'_>, id: &str) -> Result<&'a Rule> {
    opts.table
        .rules
        .iter()
        .find(|r| r.id == id)
        .ok_or_else(|| Error::Parse {
            path: PathBuf::from("rules/table.toml"),
            line: 0,
            message: format!("missing rule {id}"),
        })
}
fn coverage(root: &Path, directives: &[Directive], opts: &LintOptions<'_>) -> Result<Report> {
    let path = root.join("DEPLOYMENT_ORDER.md");
    let mut r = Report::new(&path);
    let rule = table_rule(opts, "R5")?;
    if !path.exists() {
        r.add(
            rule,
            Level::Blocking,
            0,
            "",
            None,
            "DEPLOYMENT_ORDER.md missing",
        );
        return Ok(r);
    }
    let deployment = folder::Deployment::parse(&read(&path)?);
    let mut picked: BTreeSet<_> = deployment.rows.iter().cloned().collect();
    for group in &deployment.groups {
        let ids = opts.rows.members(group)?;
        if ids.is_empty() {
            r.add(
                rule,
                Level::Blocking,
                0,
                "",
                Some((group, group)),
                "picked group has no members in tracker",
            );
        }
        picked.extend(ids);
    }
    if picked.is_empty() {
        r.add(
            rule,
            Level::Blocking,
            0,
            "",
            None,
            "no Rows or Groups selection",
        );
    }
    let served: BTreeSet<_> = directives
        .iter()
        .flat_map(|d| d.served().into_keys())
        .collect();
    for id in &picked {
        let row = opts.rows.get(id)?;
        let purpose = row.as_ref().map_or(id.as_str(), |r| r.purpose.as_str());
        if row.is_none() {
            r.add(
                rule,
                Level::Blocking,
                0,
                "",
                Some((id, purpose)),
                "picked row does not exist in tracker",
            );
        } else if !served.contains(id) && !deployment.deferred.contains_key(id) {
            r.add(
                rule,
                Level::Blocking,
                0,
                "",
                Some((id, purpose)),
                "picked row has no directive or deferred reason",
            );
        }
    }
    for id in served.difference(&picked) {
        let row = opts.rows.get(id)?;
        r.add(
            rule,
            Level::Blocking,
            0,
            "",
            Some((id, row.as_ref().map_or(id, |r| &r.purpose))),
            "served row was not picked",
        );
    }
    r.add(
        rule,
        Level::Note,
        0,
        "",
        None,
        &format!(
            "picked {} rows ({} groups), served {}, deferred {}",
            picked.len(),
            deployment.groups.len(),
            served.intersection(&picked).count(),
            deployment.deferred.len()
        ),
    );
    r.finish();
    Ok(r)
}
fn overlap(root: &Path, directives: &[Directive], opts: &LintOptions<'_>) -> Result<Report> {
    let mut r = Report::new(root);
    let exact = table_rule(opts, "L20")?;
    let ancestors = table_rule(opts, "L25")?;
    for (i, a) in directives.iter().enumerate() {
        let aw = paths(a.body("Writable scope"))?;
        let ai = paths(a.body("Inputs"))?;
        for b in directives.iter().skip(i + 1) {
            let bw = paths(b.body("Writable scope"))?;
            let bi = paths(b.body("Inputs"))?;
            for x in &aw {
                for y in &bw {
                    let xp = resolved(root, x);
                    let yp = resolved(root, y);
                    let xs = xp.to_string_lossy();
                    let ys = yp.to_string_lossy();
                    if xp == yp {
                        r.add(
                            exact,
                            Level::Blocking,
                            0,
                            x,
                            Some((&a.id, &a.title)),
                            &format!("same-wave writable path also owned by {}", b.id),
                        );
                    } else if prefix(&xp, &yp)
                        || prefix(&yp, &xp)
                        || pattern_path(&xs, &ys)?
                        || pattern_path(&ys, &xs)?
                    {
                        r.add(
                            ancestors,
                            Level::Blocking,
                            0,
                            x,
                            Some((&a.id, &a.title)),
                            &format!("same-wave writable prefix or glob overlaps {}", b.id),
                        );
                    }
                }
            }
            for (inputs, writes, reader, writer) in [(&ai, &bw, a, b), (&bi, &aw, b, a)] {
                for x in inputs {
                    for y in writes {
                        if resolved(root, x) == resolved(root, y) {
                            r.add(
                                exact,
                                Level::Blocking,
                                0,
                                x,
                                Some((&reader.id, &reader.title)),
                                &format!("same-wave input is written by {}", writer.id),
                            );
                        }
                    }
                }
            }
        }
    }
    r.finish();
    Ok(r)
}
fn arg<'a>(args: &'a [String], key: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == key)
        .and_then(|n| args.get(n + 1))
        .map(String::as_str)
}
pub fn route_args(kind: &str, model: &str, effort: &str) -> Result<Vec<String>> {
    // The effort is the operator's to set and is never refused by level (OD-2026-10-11-08, row RW-18-R06);
    // only a value that is not a plain word is refused, since it is passed into the harness's arguments.
    if effort.is_empty() || !effort.chars().all(|c| c.is_ascii_lowercase()) {
        return Err(crate::error::refuse(
            "L23",
            kind,
            "harness route and reasoning effort",
            "effort must be a plain word the harness takes",
        ));
    }
    match kind {
        "codex" if model == "gpt-6.1-sol" => Ok(vec![
            "-m".into(),
            model.into(),
            "-c".into(),
            format!("model_reasoning_effort=\"{effort}\""),
        ]),
        "claude" if ["sonnet", "claude-opus-5-5"].contains(&model) => Ok(vec![
            "--model".into(),
            model.into(),
            "--effort".into(),
            effort.into(),
        ]),
        "claude" if model == "opus" => Ok(vec![
            "--model".into(),
            model.into(),
            "--effort".into(),
            effort.into(),
        ]),
        "opencode" if !model.is_empty() => Ok(vec!["--model".into(), model.into()]),
        // A scripted pane (probes): its program is the route's model; it starts no model.
        "shell" if !model.is_empty() => Ok(vec![model.into()]),
        _ => Err(crate::error::refuse(
            "L23",
            kind,
            "harness route and model selection",
            "harness cannot run this route",
        )),
    }
}
fn manifest_rules(
    path: &Path,
    value: &serde_json::Value,
    root: &Path,
    opts: &LintOptions<'_>,
) -> Result<Report> {
    let mut r = Report::new(path);
    let mut add = |id: &str, lv: Level, task: &str, msg: &str| -> Result<()> {
        r.add(table_rule(opts, id)?, lv, 0, "", Some((task, task)), msg);
        Ok(())
    };
    let tasks = value["tasks"].as_array().cloned().unwrap_or_default();
    let mut ids = BTreeSet::new();
    let parent = path.parent().unwrap_or(root);
    for t in &tasks {
        let id = t["id"].as_str().unwrap_or("");
        if id.is_empty() || !ids.insert(id.to_owned()) {
            add(
                "L20",
                Level::Blocking,
                id,
                "missing or duplicate manifest task id",
            )?;
        }
        let file = t["directive"].as_str().map(|p| resolved(parent, p));
        let cwd = t["cwd"].as_str().map(|p| resolved(parent, p));
        if file.as_ref().is_none_or(|p| !p.is_file()) {
            add("L20", Level::Blocking, id, "manifest directive is missing")?;
        } else if file
            .as_ref()
            .and_then(|p| p.file_stem())
            .and_then(|s| s.to_str())
            .and_then(|s| s.split('_').next())
            != Some(id)
        {
            add(
                "L20",
                Level::Blocking,
                id,
                "manifest id differs from directive filename",
            )?;
        }
        if cwd.as_ref().is_none_or(|p| !p.is_dir()) {
            add("L20", Level::Blocking, id, "manifest cwd is missing")?;
        }
        let kind = t["kind"].as_str().unwrap_or("");
        let args = t["args"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_owned))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let model = if kind == "shell" {
            args.first().map(String::as_str)
        } else if kind == "codex" {
            arg(&args, "-m").or_else(|| arg(&args, "--model"))
        } else {
            arg(&args, "--model")
        }
        .unwrap_or("");
        let effort = if kind == "shell" {
            "low"
        } else if kind == "codex" {
            args.iter()
                .find_map(|a| a.strip_prefix("model_reasoning_effort="))
                .map(|s| s.trim_matches(['\"', '\'']))
                .unwrap_or("")
        } else {
            arg(&args, "--effort").unwrap_or("")
        };
        if route_args(kind, model, effort).is_err() {
            add(
                "L23",
                Level::Blocking,
                id,
                "unsupported harness route or missing explicit model/effort",
            )?;
        }
        if kind == "claude" && model == "opus" {
            add(
                "L23",
                Level::Note,
                id,
                "bare opus alias; use explicit pinned model",
            )?;
        }
        if args.iter().any(|a| ["-p", "--print"].contains(&a.as_str())) {
            add(
                "L23",
                Level::Blocking,
                id,
                "print mode is not a native interactive route",
            )?;
        }
        if let Some(lanes) = &opts.lanes {
            let v: serde_json::Value = folder::json(lanes)?;
            if let Some(lane) = v.get(kind) {
                let allowed = lane["also"].as_array().is_some_and(|a| {
                    a.iter()
                        .any(|p| p["model"] == model && p["effort"] == effort)
                });
                if !allowed && (lane["model"] != model || lane["effort"] != effort) {
                    let historical = root.join("artifacts").join(id).join("freeze.json").exists();
                    add(
                        "O8",
                        if historical {
                            Level::Note
                        } else {
                            Level::Blocking
                        },
                        id,
                        "model or effort differs from lanes file",
                    )?;
                }
            }
        }
        if kind == "codex"
            && let Some(file) = file.filter(|p| p.is_file())
        {
            let d = Directive::open(&file)?;
            if root.join("INTERFACE_PATHS.txt").exists() {
                for glob in read(&root.join("INTERFACE_PATHS.txt"))?
                    .lines()
                    .map(str::trim)
                    .filter(|g| !g.is_empty() && !g.starts_with('#'))
                {
                    for writable in paths(d.body("Writable scope"))? {
                        if pattern_path(glob, &writable)? {
                            add(
                                "O3",
                                Level::Blocking,
                                id,
                                "Codex writable path matches graphical interface glob",
                            )?;
                        }
                    }
                }
            }
        }
    }
    if let Some(depts) = value["departments"].as_array() {
        let mut widths = 0u64;
        let mut dept_ids = BTreeSet::new();
        let mut memberships = BTreeMap::<String, usize>::new();
        for dept in depts {
            let name = dept["id"]
                .as_str()
                .or_else(|| dept["name"].as_str())
                .unwrap_or("");
            if !dept_ids.insert(name) {
                add("L24", Level::Blocking, name, "duplicate department")?;
            }
            let width = dept["width"].as_u64().unwrap_or(0);
            if width == 0 {
                add(
                    "L24",
                    Level::Blocking,
                    name,
                    "department width must be positive",
                )?;
            }
            widths += width;
            if let Some(list) = dept["tasks"].as_array() {
                for id in list.iter().filter_map(|v| v.as_str()) {
                    *memberships.entry(id.into()).or_default() += 1;
                    if !ids.contains(id) {
                        add(
                            "L21",
                            Level::Blocking,
                            id,
                            "department references absent task",
                        )?;
                    }
                }
            }
        }
        for id in &ids {
            if memberships.get(id) != Some(&1) {
                add(
                    "L21",
                    Level::Blocking,
                    id,
                    "task belongs to zero or multiple departments",
                )?;
            }
        }
        if value["parallel"].as_u64() != Some(widths) {
            add(
                "L24",
                Level::Blocking,
                "manifest",
                "parallel differs from sum of department widths",
            )?;
        }
    } else if value["parallel"]
        .as_u64()
        .is_none_or(|p| p == 0 || p > tasks.len() as u64)
    {
        add(
            "L24",
            Level::Blocking,
            "manifest",
            "parallel is missing, zero, or above task count",
        )?;
    }
    if let Some(charter) = &opts.charter {
        let text = read(charter)?;
        for task in re(r"\bT\d+(?:W\d+)?\b")?.find_iter(&text) {
            if !ids.contains(task.as_str()) {
                add(
                    "L22",
                    Level::Blocking,
                    task.as_str(),
                    "charter task absent from manifest",
                )?;
            }
        }
        if let Some(want) = value["charter_sha256"].as_str()
            && sha256(charter)? != want
        {
            add("L24", Level::Blocking, "manifest", "charter hash differs")?;
        }
        // Flash charter structured keys, when present, are compared without rewriting the charter.
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
            for key in ["max_agents", "departments"] {
                if !v[key].is_null() && value[key] != v[key] {
                    add(
                        "L24",
                        Level::Blocking,
                        "manifest",
                        &format!("charter {key} differs"),
                    )?;
                }
            }
        }
    }
    r.finish();
    Ok(r)
}
fn sha256(path: &Path) -> Result<String> {
    use sha2::Digest;
    let bytes = std::fs::read(path).map_err(|e| crate::error::io(path, e))?;
    Ok(sha2::Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}
pub fn lint(path: &Path, opts: &LintOptions<'_>) -> Result<Findings> {
    let root = root_of(path, opts)?;
    let mut out = Findings::default();
    let mut directives = Vec::new();
    if path.is_file() {
        if path.extension().and_then(|s| s.to_str()) == Some("json") {
            let value: serde_json::Value = folder::json(path)?;
            let manifest: Manifest =
                serde_json::from_value(value.clone()).map_err(|e| Error::Parse {
                    path: path.into(),
                    line: 0,
                    message: e.to_string(),
                })?;
            for t in &manifest.tasks {
                let file = if t.directive.is_absolute() {
                    t.directive.clone()
                } else {
                    path.parent().unwrap_or(&root).join(&t.directive)
                };
                if file.exists() {
                    directives.push(Directive::open(&file)?);
                }
            }
            out.reports.push(manifest_rules(path, &value, &root, opts)?);
            out.reports.push(overlap(&root, &directives, opts)?);
        } else {
            directives.push(Directive::open(path)?);
        }
    } else if path.join("OUTCOME.md").exists()
        || !path
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.starts_with("wave"))
    {
        root_files(path, opts, &mut out)?;
        let mut ids = BTreeSet::new();
        for p in folder::entries(path)? {
            if p.is_dir()
                && p.file_name()
                    .and_then(|s| s.to_str())
                    .and_then(|s| s.strip_prefix("wave"))
                    .is_some_and(|s| s.parse::<u32>().is_ok())
            {
                let batch = folder::entries(&p)?
                    .iter()
                    .filter(|p| folder::task_file(p))
                    .map(|p| Directive::open(p))
                    .collect::<Result<Vec<_>>>()?;
                for d in &batch {
                    if !ids.insert(d.id.clone()) {
                        let mut r = Report::new(&d.path);
                        r.add(
                            table_rule(opts, "L20")?,
                            Level::Blocking,
                            0,
                            "",
                            Some((&d.id, &d.title)),
                            "campaign-wide task id reused",
                        );
                        out.reports.push(r);
                    }
                }
                out.reports.push(overlap(path, &batch, opts)?);
                directives.extend(batch);
            }
        }
        for p in folder::entries(path)?.into_iter().filter(|p| {
            p.is_file()
                && p.file_name()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| s.starts_with("wave") && s.ends_with(".json"))
        }) {
            let value = folder::json(&p)?;
            out.reports.push(manifest_rules(&p, &value, path, opts)?);
        }
        if opts.coverage && opts.row_checks {
            out.reports.push(coverage(path, &directives, opts)?);
        }
    } else {
        directives = folder::entries(path)?
            .iter()
            .filter(|p| folder::task_file(p))
            .map(|p| Directive::open(p))
            .collect::<Result<Vec<_>>>()?;
        out.reports.push(overlap(&root, &directives, opts)?);
        // A wave checked with its campaign root named carries the campaign's own checks too, so
        // a pack refused by the outcome check is refused here by the same rule (RW-21-R03).
        if opts.campaign_root.is_some() {
            root_files(&root, opts, &mut out)?;
            if opts.coverage && opts.row_checks {
                out.reports
                    .push(coverage(&root, &campaign_directives(&root)?, opts)?);
            }
        }
        for p in folder::entries(path)?.into_iter().filter(|p| {
            p.extension().and_then(|s| s.to_str()) == Some("json")
                && p.file_name()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| s.starts_with("swarm"))
        }) {
            let value = folder::json(&p)?;
            out.reports.push(manifest_rules(&p, &value, &root, opts)?);
        }
    }
    for d in directives {
        out.reports.push(lint_directive(&d, &root, opts)?);
    }
    out.finish();
    Ok(out)
}
/// O1 and O3: the campaign root holds OUTCOME.md and INTERFACE_PATHS.txt.
fn root_files(path: &Path, opts: &LintOptions<'_>, out: &mut Findings) -> Result<()> {
    if !path.join("OUTCOME.md").exists() {
        let mut r = Report::new(&path.join("OUTCOME.md"));
        r.add(
            table_rule(opts, "O1")?,
            Level::Blocking,
            0,
            "",
            None,
            "OUTCOME.md missing at campaign root",
        );
        out.reports.push(r);
    }
    if !path.join("INTERFACE_PATHS.txt").exists() {
        let mut r = Report::new(&path.join("INTERFACE_PATHS.txt"));
        r.add(
            table_rule(opts, "O3")?,
            Level::Blocking,
            0,
            "",
            None,
            "INTERFACE_PATHS.txt missing at campaign root",
        );
        out.reports.push(r);
    }
    Ok(())
}
/// Every directive under the campaign's wave folders, for the campaign-wide row coverage.
fn campaign_directives(root: &Path) -> Result<Vec<Directive>> {
    let mut all = Vec::new();
    for p in folder::entries(root)? {
        if p.is_dir()
            && p.file_name()
                .and_then(|s| s.to_str())
                .and_then(|s| s.strip_prefix("wave"))
                .is_some_and(|s| s.parse::<u32>().is_ok())
        {
            for f in folder::entries(&p)?.iter().filter(|p| folder::task_file(p)) {
                all.push(Directive::open(f)?);
            }
        }
    }
    Ok(all)
}
/// Explicit after-build graphical-file check. The caller supplies only authored paths, preserving read scope.
pub fn inspect_owned(
    root: &Path,
    task: &str,
    kind: &str,
    paths: &[PathBuf],
    opts: &LintOptions<'_>,
) -> Result<Findings> {
    let mut out = Findings::default();
    let globs = read(&root.join("INTERFACE_PATHS.txt"))?;
    let freeze = root.join("artifacts").join(task).join("freeze.json");
    let seal: serde_json::Value = if freeze.exists() {
        folder::json(&freeze)?
    } else {
        serde_json::Value::Null
    };
    for p in paths {
        let s = p.to_string_lossy();
        if !globs
            .lines()
            .filter(|g| !g.starts_with('#') && !g.trim().is_empty())
            .any(|g| pattern_path(g.trim(), &s).unwrap_or(false))
        {
            continue;
        }
        let frozen = seal["entries"].as_array().is_some_and(|entries| {
            entries.iter().any(|e| {
                let candidate = seal["candidate"].as_str().unwrap_or("");
                e["path"]
                    .as_str()
                    .is_some_and(|rel| Path::new(candidate).join(rel) == *p)
                    && e["sha256"]
                        .as_str()
                        .is_some_and(|h| sha256(p).ok().as_deref() == Some(h))
            })
        });
        if frozen {
            continue;
        }
        let mut r = Report::new(p);
        if kind == "codex" {
            r.add(
                table_rule(opts, "O5")?,
                Level::Blocking,
                0,
                "",
                Some((task, task)),
                "Codex authored file matches graphical interface glob",
            );
        }
        if !s.contains("/tests/") && !s.contains("/fixtures/") {
            for (n, l) in read(p)?.lines().enumerate() {
                let code = l.trim_start();
                let comment =
                    code.starts_with("//") || code.starts_with('#') || code.starts_with('*');
                if re(r"\b(TODO|FIXME)\b")?.is_match(l)
                    || (!comment
                        && re(r#"["'][^"']*(?:placeholder|/home/)[^"']*["']"#)?.is_match(l))
                {
                    r.add(
                        table_rule(opts, "O4")?,
                        Level::Blocking,
                        n + 1,
                        l,
                        Some((task, task)),
                        "displayed developer note or absolute home path",
                    );
                }
            }
        }
        r.finish();
        out.reports.push(r);
    }
    out.finish();
    Ok(out)
}
/// Optional constitution integrity check on caller-provided, explicitly scoped files.
pub fn check_constitution(
    manifest: &Path,
    tree: &Path,
    opts: &LintOptions<'_>,
) -> Result<Findings> {
    let mut out = Findings::default();
    let text = read(manifest)?;
    let mut known = BTreeSet::new();
    for l in text.lines() {
        if let Some((hash, rel)) = l.split_once("  ") {
            let p = tree.join(rel);
            known.insert(p.clone());
            let mut r = Report::new(&p);
            if !p.exists() || sha256(&p)? != hash {
                r.add(
                    table_rule(opts, "O7")?,
                    Level::Blocking,
                    0,
                    "",
                    None,
                    "constitution file missing or changed against manifest",
                );
            }
            out.reports.push(r);
        }
    }
    fn walk(
        p: &Path,
        known: &BTreeSet<PathBuf>,
        out: &mut Findings,
        opts: &LintOptions<'_>,
    ) -> Result<()> {
        for p in folder::entries(p)? {
            if p.is_dir() {
                walk(&p, known, out, opts)?;
            } else if !known.contains(&p) {
                let mut r = Report::new(&p);
                r.add(
                    table_rule(opts, "O7")?,
                    Level::Blocking,
                    0,
                    "",
                    None,
                    "constitution file added against manifest",
                );
                out.reports.push(r);
            }
        }
        Ok(())
    }
    walk(tree, &known, &mut out, opts)?;
    out.finish();
    Ok(out)
}

#[cfg(test)]
mod effort_tests {
    #[test]
    fn every_effort_level_is_a_route() {
        let codex = super::route_args("codex", "gpt-6.1-sol", "high").unwrap();
        assert!(codex.contains(&"model_reasoning_effort=\"high\"".to_string()));
        for effort in ["none", "low", "medium", "high", "xhigh", "max"] {
            assert!(super::route_args("codex", "gpt-6.1-sol", effort).is_ok());
            assert!(super::route_args("claude", "claude-opus-5-5", effort).is_ok());
        }
        assert!(super::route_args("codex", "gpt-6.1-sol", "").is_err());
        assert!(super::route_args("codex", "gpt-6.1-sol", "high\" -c x").is_err());
    }
}
