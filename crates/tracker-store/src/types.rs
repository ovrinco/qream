use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub type Fields = Map<String, Value>;
pub type Row = Fields;
pub type Event = Row;
pub type Usage = Fields;

#[derive(Debug, Serialize, Deserialize)]
pub struct Page {
    pub rows: Vec<Row>,
    pub total: usize,
    pub limit: usize,
    pub offset: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Assignment {
    #[serde(default)]
    pub seq: i64,
    pub campaign: String,
    pub wave: i64,
    pub task: String,
    pub directive: String,
    #[serde(default)]
    pub row_id: String,
    pub part: i64,
    pub state: String,
    #[serde(default)]
    pub at: String,
    #[serde(default)]
    pub author: String,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Duration {
    pub campaign: String,
    pub task: String,
    pub kind: String,
    pub started_at: String,
    pub ended_at: String,
    pub seconds: f64,
    pub outcome: String,
    pub note: String,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Fault {
    pub seen: String,
    pub area: String,
    pub fault: String,
    pub effect: String,
    pub source: String,
    pub status: String,
    pub fix: String,
    pub line: i64,
}
#[derive(Clone, Copy, Debug)]
pub enum Export {
    Xlsx,
    Csv,
    Tsv,
    Parquet,
}

pub(crate) const REQ_COLS: &[&str] = &[
    "id",
    "tab",
    "item_type",
    "status",
    "purpose",
    "intent",
    "dod",
    "notes",
    "overlap",
    "goal_deadline",
    "surface",
    "source",
    "said_on",
    "exemplar",
    "implemented_in",
    "age",
    "outcome",
    "outcome_id",
];
pub(crate) const REQ_HEADERS: &[&str] = &[
    "ID",
    "TAB",
    "ITEM TYPE",
    "STATUS",
    "PURPOSE",
    "INTENT",
    "D.O.D.",
    "NOTES",
    "OVERLAP",
    "GOAL/DEADLINE",
    "SURFACE",
    "SOURCE",
    "SAID ON",
    "EXEMPLAR",
    "IMPLEMENTED IN",
    "AGE",
    "OUTCOME",
    "OUTCOME ID",
];
pub(crate) const CAMP_COLS: &[&str] = &[
    "campaign",
    "campaign_state",
    "campaign_evidence",
    "campaign_remaining",
    "campaign_updated_at",
];
pub(crate) const CAMP_HEADERS: &[&str] = &[
    "CAMPAIGN",
    "CAMPAIGN STATE",
    "CAMPAIGN EVIDENCE",
    "CAMPAIGN REMAINING",
    "CAMPAIGN UPDATED AT",
];
pub(crate) const OPEN: &[&str] = &[
    "said",
    "proposed",
    "Planning",
    "planned",
    "in build",
    "built wrong",
];
pub(crate) const ASSIGN_STATES: &[&str] =
    &["planned", "in build", "blocked", "implemented", "verified"];
pub(crate) const CAMP_STATES: &[&str] = &[
    "Not assessed",
    "In progress",
    "Blocked",
    "Candidate proven",
    "Verified",
    "Context / not commissioned",
];
pub(crate) const USAGE_FIELDS: &[&str] = &[
    "campaign",
    "role",
    "unit",
    "run",
    "tokens_in",
    "tokens_cached",
    "tokens_out",
    "cpu_s",
    "wall_s",
    "outcome",
    "detail",
];

pub(crate) fn field(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    let out = out.trim_matches('_');
    match out {
        "d_o_d" => "dod".into(),
        "" => "col".into(),
        _ => out.into(),
    }
}
pub(crate) fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}
pub(crate) fn text(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        _ => v.to_string(),
    }
}
pub(crate) fn active(v: Option<&Value>) -> bool {
    v.and_then(Value::as_str).is_some_and(|s| OPEN.contains(&s))
}
/// Python's json.dumps layout, including one-space pretty indentation and
/// insertion order. Event cells use ASCII escaping; command output uses UTF-8.
pub(crate) fn py_json(v: &Value, pretty: bool, ascii: bool) -> String {
    fn string(s: &str, ascii: bool) -> String {
        let mut out = String::from("\"");
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                '\u{8}' => out.push_str("\\b"),
                '\u{c}' => out.push_str("\\f"),
                c if c < ' ' || (ascii && !c.is_ascii()) => {
                    let mut buf = [0; 2];
                    for unit in c.encode_utf16(&mut buf) {
                        out.push_str(&format!("\\u{unit:04x}"));
                    }
                }
                c => out.push(c),
            }
        }
        out.push('"');
        out
    }
    fn go(v: &Value, pretty: bool, ascii: bool, depth: usize) -> String {
        let (left, right, entries) = match v {
            Value::Array(a) => (
                "[",
                "]",
                a.iter()
                    .map(|v| go(v, pretty, ascii, depth + 1))
                    .collect::<Vec<_>>(),
            ),
            Value::Object(o) => (
                "{",
                "}",
                o.iter()
                    .map(|(k, v)| {
                        format!("{}: {}", string(k, ascii), go(v, pretty, ascii, depth + 1))
                    })
                    .collect(),
            ),
            Value::String(s) => return string(s, ascii),
            _ => return v.to_string(),
        };
        if entries.is_empty() {
            return format!("{left}{right}");
        }
        if pretty {
            format!(
                "{left}\n{}{}\n{}{right}",
                " ".repeat(depth + 1),
                entries.join(&format!(",\n{}", " ".repeat(depth + 1))),
                " ".repeat(depth)
            )
        } else {
            format!("{left}{}{right}", entries.join(", "))
        }
    }
    go(v, pretty, ascii, 0)
}
