//! The stable disk interface, without registry or daemon dependencies.
use crate::{
    Error, Result,
    error::{io, read},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
pub const SECTIONS: [&str; 9] = [
    "Orientation",
    "Rows",
    "Inputs",
    "Writable scope",
    "Work items",
    "Shared resources",
    "Acceptance checklist",
    "Output",
    "Blockers",
];
pub const FILES: [&str; 10] = [
    "OUTCOME.md",
    "INTERFACE_PATHS.txt",
    "DEPLOYMENT_ORDER.md",
    "INPUT_FREEZE.md",
    "PRE_FLIGHT.md",
    "CLOSE.json",
    "ROADMAP.md",
    "RETROSPECTIVE.md",
    "FINAL_ACCEPTANCE_PACKET.md",
    "status.md",
];
pub const DIRS: [&str; 9] = [
    "reports",
    "artifacts",
    "work",
    "design",
    "spec",
    "state",
    "controller",
    "reviews",
    "backlog",
];
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Section {
    pub name: String,
    pub line: usize,
    pub body: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Directive {
    pub id: String,
    pub path: PathBuf,
    pub title: String,
    pub text: String,
    pub sections: Vec<Section>,
}
impl Directive {
    pub fn parse(path: &Path, text: String) -> Result<Self> {
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let id = stem.split('_').next().unwrap_or("").to_owned();
        let mut sections: Vec<Section> = Vec::new();
        for (n, l) in text.lines().enumerate() {
            if let Some(h) = l.strip_prefix("## ") {
                sections.push(Section {
                    name: h.into(),
                    line: n + 1,
                    body: String::new(),
                });
            } else if let Some(s) = sections.last_mut() {
                s.body.push_str(l);
                s.body.push('\n');
            }
        }
        let title = text
            .lines()
            .find_map(|l| l.strip_prefix("# "))
            .unwrap_or("")
            .to_owned();
        Ok(Self {
            id,
            path: path.into(),
            title,
            text,
            sections,
        })
    }
    pub fn open(path: &Path) -> Result<Self> {
        Self::parse(path, read(path)?)
    }
    pub fn section(&self, name: &str) -> Option<&Section> {
        self.sections
            .iter()
            .find(|s| s.name.to_lowercase().starts_with(&name.to_lowercase()))
    }
    pub fn body(&self, name: &str) -> &str {
        self.section(name).map(|s| s.body.as_str()).unwrap_or("")
    }
    pub fn served(&self) -> BTreeMap<String, bool> {
        let mut out = BTreeMap::new();
        for line in self.text.lines() {
            let line = line
                .trim()
                .trim_start_matches(['-', '*', '#', ' '])
                .replace("**", "");
            if !line.to_lowercase().starts_with("serves rows:") {
                continue;
            }
            for tok in line
                .split_once(':')
                .map(|(_, s)| s)
                .unwrap_or("")
                .split([',', ';'])
            {
                let part = tok.contains("(part)");
                let clean = tok.replace("(part)", "");
                let id = clean
                    .trim()
                    .trim_matches(['`', '*', '(', ')', '[', ']', '.', ' ']);
                if id.len() > 2
                    && id.contains('-')
                    && id.chars().any(|c| c.is_ascii_digit())
                    && id
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
                {
                    out.entry(id.into()).or_insert(part);
                }
            }
        }
        out
    }
    pub fn row_fields(&self) -> BTreeMap<String, BTreeMap<String, String>> {
        let mut out = BTreeMap::new();
        let mut id = None;
        let mut field: Option<String> = None;
        for l in self.body("Rows").lines() {
            if let Some(h) = l.strip_prefix("### ") {
                id = h.split_whitespace().next().map(str::to_owned);
                field = None;
                if let Some(i) = &id {
                    out.insert(i.clone(), BTreeMap::<String, String>::new());
                }
                continue;
            }
            let Some(i) = &id else {
                continue;
            };
            let Some(fields) = out.get_mut(i) else {
                continue;
            };
            if let Some((label, body)) = l.split_once(':')
                && [
                    "Purpose",
                    "Intent",
                    "Definition of done",
                    "Exemplar",
                    "Source",
                ]
                .contains(&label)
            {
                fields.insert(label.into(), body.trim().into());
                field = Some(label.into());
                continue;
            }
            if let Some(f) = &field
                && let Some(v) = fields.get_mut(f)
                && !l.trim().is_empty()
            {
                v.push(' ');
                v.push_str(l.trim());
            }
        }
        out
    }
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Deployment {
    pub rows: Vec<String>,
    pub groups: Vec<String>,
    pub deferred: BTreeMap<String, String>,
    pub close: String,
}
impl Deployment {
    pub fn parse(text: &str) -> Self {
        let mut d = Self::default();
        let mut deferred = false;
        let mut close = false;
        for l in text.lines() {
            for (label, list) in [("Rows:", &mut d.rows), ("Groups:", &mut d.groups)] {
                if let Some(s) = l.strip_prefix(label) {
                    list.extend(
                        s.split([',', ';'])
                            .map(|x| x.trim().trim_matches('`').to_owned())
                            .filter(|x| !x.is_empty()),
                    );
                }
            }
            if l.starts_with("## ") {
                close = l.trim() == "## Close";
                deferred = l.trim() == "## Deferred";
            }
            if l.trim() == "Deferred:" {
                deferred = true;
                continue;
            }
            if close {
                d.close.push_str(l);
                d.close.push('\n');
            }
            if deferred {
                if let Some(s) = l
                    .trim()
                    .strip_prefix("- ")
                    .or_else(|| l.trim().strip_prefix("* "))
                {
                    if let Some((id, reason)) = s.split_once(':')
                        && !reason.trim().is_empty()
                    {
                        d.deferred.insert(id.trim().into(), reason.trim().into());
                    }
                } else if !l.trim().is_empty() {
                    deferred = false;
                }
            }
        }
        d
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Close {
    pub campaign: String,
    pub last_wave: u32,
    pub declared_at: String,
    pub expected_close: String,
    pub by: String,
    pub rule: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual_close: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestTask {
    pub id: String,
    pub cwd: PathBuf,
    pub directive: PathBuf,
    pub kind: String,
    pub args: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub name: String,
    pub parallel: usize,
    pub campaign: String,
    pub tasks: Vec<ManifestTask>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Wave {
    pub number: u32,
    pub path: PathBuf,
    pub directives: Vec<Directive>,
    pub manifest: Option<Manifest>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Campaign {
    pub root: PathBuf,
    pub outcome: String,
    pub deployment: Deployment,
    pub close: Option<Close>,
    pub waves: Vec<Wave>,
    pub reports: Vec<PathBuf>,
    pub seals: BTreeMap<String, serde_json::Value>,
}
pub fn json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    serde_json::from_str(&read(path)?).map_err(|e| Error::Parse {
        path: path.into(),
        line: e.line(),
        message: e.to_string(),
    })
}
pub fn entries(path: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(path).map_err(|e| io(path, e))? {
        out.push(e.map_err(|e| io(path, e))?.path());
    }
    out.sort();
    Ok(out)
}
pub fn task_file(p: &Path) -> bool {
    p.file_name().and_then(|s| s.to_str()).is_some_and(|s| {
        s.ends_with(".md")
            && s.split_once('_').is_some_and(|(id, _)| {
                id.starts_with('T') && id.chars().nth(1).is_some_and(|c| c.is_ascii_digit())
            })
    })
}
impl Campaign {
    pub fn open(root: &Path) -> Result<Self> {
        let root = std::fs::canonicalize(root).map_err(|e| io(root, e))?;
        let outcome = read(&root.join("OUTCOME.md"))?;
        let deployment = Deployment::parse(&read(&root.join("DEPLOYMENT_ORDER.md"))?);
        let close = if root.join("CLOSE.json").exists() {
            let value: serde_json::Value = json(&root.join("CLOSE.json"))?;
            if value.is_null() {
                None
            } else {
                Some(json(&root.join("CLOSE.json"))?)
            }
        } else {
            None
        };
        let mut waves = Vec::new();
        for p in entries(&root)? {
            if p.is_dir()
                && let Some(n) = p
                    .file_name()
                    .and_then(|s| s.to_str())
                    .and_then(|s| s.strip_prefix("wave"))
                    .and_then(|s| s.parse::<u32>().ok())
            {
                let directives = entries(&p)?
                    .iter()
                    .filter(|p| task_file(p))
                    .map(|p| Directive::open(p))
                    .collect::<Result<Vec<_>>>()?;
                let mp = root.join(format!("wave{n}.json"));
                let manifest = if mp.exists() { Some(json(&mp)?) } else { None };
                waves.push(Wave {
                    number: n,
                    path: p,
                    directives,
                    manifest,
                });
            }
        }
        waves.sort_by_key(|w| w.number);
        let reports = if root.join("reports").is_dir() {
            entries(&root.join("reports"))?
        } else {
            Vec::new()
        };
        let mut seals = BTreeMap::new();
        if root.join("artifacts").is_dir() {
            for p in entries(&root.join("artifacts"))? {
                if p.join("freeze.json").exists() {
                    seals.insert(
                        p.file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned(),
                        json(&p.join("freeze.json"))?,
                    );
                }
            }
        }
        Ok(Self {
            root,
            outcome,
            deployment,
            close,
            waves,
            reports,
            seals,
        })
    }
}
