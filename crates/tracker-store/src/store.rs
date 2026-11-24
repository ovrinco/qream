use crate::types::*;
use crate::{Error, Result};
use chrono::{SecondsFormat, Utc};
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior, params,
    params_from_iter,
    types::{Value as SqlValue, ValueRef},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::Path;

pub struct Store {
    pub(crate) db: Connection,
    author: String,
}
type Source = (String, String, String, Vec<String>, Option<String>);
pub(crate) type Query = (Vec<String>, Vec<Vec<Value>>);
fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, false)
}
fn sql_value(v: &Value) -> Result<SqlValue> {
    Ok(match v {
        Value::Null => SqlValue::Null,
        Value::String(s) => SqlValue::Text(s.clone()),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                SqlValue::Integer(i)
            } else if let Some(f) = n.as_f64() {
                SqlValue::Real(f)
            } else {
                return Err(Error::Invalid("number outside SQLite range".into()));
            }
        }
        Value::Bool(b) => SqlValue::Integer(i64::from(*b)),
        _ => return Err(Error::Invalid("fields must be scalar values".into())),
    })
}
impl Store {
    pub fn open(path: &Path, author: &str) -> Result<Self> {
        if !path.is_file() {
            return Err(Error::Invalid(format!(
                "no tracker database at {}; select an existing tracker database with --db or TRACKER_DB",
                path.display()
            )));
        }
        if author.trim().is_empty() {
            return Err(Error::Invalid("author must be nonempty".into()));
        }
        let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON;",
        )?;
        let tx = Transaction::new_unchecked(&db, TransactionBehavior::Immediate)?;
        tx.execute_batch("
            CREATE TABLE IF NOT EXISTS duration(id INTEGER PRIMARY KEY AUTOINCREMENT,campaign TEXT,task TEXT,kind TEXT CHECK(kind IN ('build','test','check','install','task')),started_at TEXT,ended_at TEXT,seconds REAL,outcome TEXT,note TEXT);
            CREATE TABLE IF NOT EXISTS fault(id INTEGER PRIMARY KEY AUTOINCREMENT,seen TEXT,area TEXT,fault TEXT,effect TEXT,source TEXT,status TEXT,fix TEXT,line INTEGER);
        ")?;
        Self::migrate_duration_v1(&tx)?;
        tx.commit()?;
        Ok(Self {
            db,
            author: author.into(),
        })
    }
    /// Schema version 1 expands duration kinds without changing any existing row or id.
    /// The replacement, auxiliary schema and AUTOINCREMENT high-water mark commit together.
    fn migrate_duration_v1(tx: &Transaction<'_>) -> Result<()> {
        let version: u32 = tx.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        let schema: String = tx.query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='duration'",
            [],
            |r| r.get(0),
        )?;
        if !schema.contains("'task'") {
            if version > 1 {
                return Err(Error::Invalid(
                    "newer store schema has no task duration kind".into(),
                ));
            }
            let sequence: Option<i64> = tx
                .query_row(
                    "SELECT seq FROM sqlite_sequence WHERE name='duration'",
                    [],
                    |r| r.get(0),
                )
                .optional()?;
            let auxiliary = tx.prepare(
                "SELECT sql FROM sqlite_master WHERE tbl_name='duration' AND type IN ('index','trigger') AND sql IS NOT NULL ORDER BY type,name",
            )?.query_map([], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            tx.execute_batch("CREATE TABLE duration_v1(id INTEGER PRIMARY KEY AUTOINCREMENT,campaign TEXT,task TEXT,kind TEXT CHECK(kind IN ('build','test','check','install','task')),started_at TEXT,ended_at TEXT,seconds REAL,outcome TEXT,note TEXT);
                INSERT INTO duration_v1 SELECT id,campaign,task,kind,started_at,ended_at,seconds,outcome,note FROM duration;
                DROP TABLE duration;
                ALTER TABLE duration_v1 RENAME TO duration;")?;
            for sql in auxiliary {
                tx.execute_batch(&sql)?;
            }
            match sequence {
                Some(seq) => {
                    tx.execute(
                        "UPDATE sqlite_sequence SET seq=max(seq,?1) WHERE name='duration'",
                        [seq],
                    )?;
                }
                None => {
                    tx.execute("DELETE FROM sqlite_sequence WHERE name='duration'", [])?;
                }
            }
        }
        if version < 1 {
            tx.execute_batch("PRAGMA user_version=1;")?;
        }
        Ok(())
    }
    /// Open for reading only: no pragma, no table, no write reaches the file.
    pub fn open_read_only(path: &Path) -> Result<Self> {
        if !path.is_file() {
            return Err(Error::Invalid(format!(
                "no tracker database at {}",
                path.display()
            )));
        }
        let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(Self {
            db,
            author: "read-only".into(),
        })
    }
    fn tx(&self) -> Result<Transaction<'_>> {
        Ok(Transaction::new_unchecked(
            &self.db,
            TransactionBehavior::Immediate,
        )?)
    }
    pub fn revision(&self) -> Result<u64> {
        Ok(self
            .db
            .query_row("SELECT coalesce(max(seq),0) FROM event", [], |r| r.get(0))?)
    }
    pub(crate) fn query_params(&self, sql: &str, args: &[SqlValue]) -> Result<Query> {
        let mut stmt = self.db.prepare(sql)?;
        // CLI query is a read-only lens, never a bypass around logged writes.
        if !stmt.readonly() {
            return Err(Error::Invalid(
                "query must be read-only; use tracker mutation verbs".into(),
            ));
        }
        let cols: Vec<String> = stmt.column_names().map(str::to_owned).collect();
        let n = cols.len();
        let rows = stmt
            .query_map(params_from_iter(args), |r| {
                (0..n)
                    .map(|i| {
                        Ok(match r.get_ref(i)? {
                            ValueRef::Null => Value::Null,
                            ValueRef::Integer(i) => json!(i),
                            ValueRef::Real(f) => json!(f),
                            ValueRef::Text(s) => Value::String(
                                std::str::from_utf8(s)
                                    .map_err(|e| {
                                        rusqlite::Error::FromSqlConversionFailure(
                                            i,
                                            rusqlite::types::Type::Text,
                                            Box::new(e),
                                        )
                                    })?
                                    .into(),
                            ),
                            ValueRef::Blob(_) => {
                                return Err(rusqlite::Error::InvalidColumnType(
                                    i,
                                    cols[i].clone(),
                                    rusqlite::types::Type::Blob,
                                ));
                            }
                        })
                    })
                    .collect::<rusqlite::Result<Vec<_>>>()
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok((cols, rows))
    }
    pub fn query(&self, sql: &str) -> Result<Query> {
        self.query_params(sql, &[])
    }
    pub(crate) fn objects(&self, sql: &str, args: &[SqlValue]) -> Result<Vec<Row>> {
        let (cols, rows) = self.query_params(sql, args)?;
        Ok(rows
            .into_iter()
            .map(|r| cols.iter().cloned().zip(r).collect())
            .collect())
    }
    fn has(&self, table: &str) -> Result<bool> {
        Ok(self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?)",
            [table],
            |r| r.get(0),
        )?)
    }
    pub(crate) fn locate(&self, id: &str) -> Result<(String, String, Vec<String>)> {
        self.find(id)?
            .ok_or_else(|| Error::Invalid(format!("no row with ID {id:?}")))
    }
    /// Whether a requirement, register row or fault with this ID exists.
    pub fn exists(&self, id: &str) -> Result<bool> {
        Ok(self.find(id)?.is_some())
    }
    fn find(&self, id: &str) -> Result<Option<(String, String, Vec<String>)>> {
        let exists: bool = self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM requirement WHERE id=?)",
            [id],
            |r| r.get(0),
        )?;
        if exists {
            return Ok(Some((
                "requirement".into(),
                "id".into(),
                REQ_COLS.iter().map(|s| (*s).into()).collect(),
            )));
        }
        for b in self.objects("SELECT tbl,cols FROM block", &[])? {
            let tbl = text(&b["tbl"]);
            let cols: Vec<String> = serde_json::from_str(&text(&b["cols"]))?;
            let key = cols
                .first()
                .ok_or_else(|| Error::Invalid(format!("empty register block {tbl}")))?;
            let sql = format!(
                "SELECT EXISTS(SELECT 1 FROM {} WHERE {}=?)",
                quote(&tbl),
                quote(key)
            );
            if self.db.query_row(&sql, [id], |r| r.get::<_, bool>(0))? {
                return Ok(Some((tbl, key.clone(), cols)));
            }
        }
        if self
            .db
            .query_row("SELECT EXISTS(SELECT 1 FROM fault WHERE id=?)", [id], |r| {
                r.get::<_, bool>(0)
            })?
        {
            return Ok(Some((
                "fault".into(),
                "id".into(),
                vec![
                    "id", "seen", "area", "fault", "effect", "source", "status", "fix", "line",
                ]
                .into_iter()
                .map(String::from)
                .collect(),
            )));
        }
        Ok(None)
    }
    fn row_at(&self, table: &str, key: &str, id: &str) -> Result<Row> {
        self.objects(
            &format!("SELECT * FROM {} WHERE {}=?", quote(table), quote(key)),
            &[SqlValue::Text(id.into())],
        )?
        .into_iter()
        .next()
        .ok_or_else(|| Error::Invalid(format!("no row with ID {id:?}")))
    }
    fn campaign_cells(&self, row: &mut Row, id: &str) -> Result<()> {
        if let Some(c) = self
            .objects(
                "SELECT * FROM campaign WHERE id=?",
                &[SqlValue::Text(id.into())],
            )?
            .first()
        {
            for k in CAMP_COLS {
                row.insert((*k).into(), c[*k].clone());
            }
        }
        Ok(())
    }
    pub fn get(&self, id: &str) -> Result<Row> {
        let (table, key, _) = self.locate(id)?;
        let mut row = self.row_at(&table, &key, id)?;
        if table == "requirement" {
            self.campaign_cells(&mut row, id)?;
            row.insert(
                "evidence".into(),
                json!(self.objects(
                    "SELECT path,sha256,note,author,at FROM evidence WHERE id=? ORDER BY seq",
                    &[SqlValue::Text(id.into())]
                )?),
            );
        }
        row.insert("assignments".into(), serde_json::to_value(self.links(id)?)?);
        row.insert("_table".into(), json!(table));
        Ok(row)
    }
    pub fn group(&self, id: &str) -> Result<Row> {
        let row = self.get(id)?;
        if row.get("_table").and_then(Value::as_str) != Some("t_outcomes_2") {
            return Err(Error::Invalid(format!("{id} is not an outcome group")));
        }
        Ok(row)
    }
    pub fn members(&self, id: &str) -> Result<Vec<String>> {
        Ok(self
            .objects(
                "SELECT id FROM requirement WHERE outcome_id=? ORDER BY sheet,position,id",
                &[SqlValue::Text(id.into())],
            )?
            .iter()
            .map(|r| text(&r["id"]))
            .collect())
    }
    pub fn list(
        &self,
        tab: Option<&str>,
        status: Option<&str>,
        search: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Page> {
        let mut sources: Vec<Source> = vec![(
            "requirement".into(),
            "id".into(),
            "position".into(),
            REQ_COLS
                .iter()
                .map(|s| (*s).into())
                .collect::<Vec<String>>(),
            None,
        )];
        for b in self.objects(
            "SELECT tbl,sheet,cols FROM block ORDER BY sheet,header_row",
            &[],
        )? {
            let cols: Vec<String> = serde_json::from_str(&text(&b["cols"]))?;
            let key = cols
                .first()
                .ok_or_else(|| Error::Invalid("empty block".into()))?
                .clone();
            sources.push((
                text(&b["tbl"]),
                key,
                "_pos".into(),
                cols,
                Some(text(&b["sheet"])),
            ));
        }
        if tab == Some("Faults") {
            sources.push((
                "fault".into(),
                "id".into(),
                "id".into(),
                vec![
                    "id", "seen", "area", "fault", "effect", "source", "status", "fix", "line",
                ]
                .into_iter()
                .map(String::from)
                .collect(),
                Some("Faults".into()),
            ));
        }
        let mut rows = Vec::new();
        let mut total = 0;
        for (table, key, pos, cols, sheet) in sources {
            if sheet
                .as_deref()
                .is_some_and(|s| tab.is_some_and(|t| s != t))
            {
                continue;
            }
            let mut conditions = Vec::new();
            let mut args = Vec::new();
            if table == "requirement"
                && let Some(t) = tab
            {
                conditions.push("sheet=?".into());
                args.push(SqlValue::Text(t.into()));
            }
            if let Some(s) = status {
                if !cols.iter().any(|c| c == "status") {
                    continue;
                }
                conditions.push("status=?".into());
                args.push(SqlValue::Text(s.into()));
            }
            if let Some(s) = search.filter(|s| !s.is_empty()) {
                conditions.push(format!(
                    "({})",
                    cols.iter()
                        .map(|c| format!(
                            "instr(lower(coalesce(CAST({} AS TEXT),'')),lower(?))>0",
                            quote(c)
                        ))
                        .collect::<Vec<_>>()
                        .join(" OR ")
                ));
                args.extend(cols.iter().map(|_| SqlValue::Text(s.into())));
            }
            let clause = if conditions.is_empty() {
                String::new()
            } else {
                format!(" WHERE {}", conditions.join(" AND "))
            };
            let count: usize = self.db.query_row(
                &format!("SELECT count(*) FROM {}{clause}", quote(&table)),
                params_from_iter(&args),
                |r| r.get(0),
            )?;
            let skip = offset.saturating_sub(total);
            if rows.len() < limit && skip < count {
                let mut a = args.clone();
                a.push(SqlValue::Integer((limit - rows.len()) as i64));
                a.push(SqlValue::Integer(skip as i64));
                let sql = format!(
                    "SELECT * FROM {}{clause} ORDER BY {}{},{} LIMIT ? OFFSET ?",
                    quote(&table),
                    if table == "requirement" { "sheet," } else { "" },
                    quote(&pos),
                    quote(&key)
                );
                for mut row in self.objects(&sql, &a)? {
                    row.insert("_table".into(), json!(table));
                    if let Some(s) = &sheet {
                        row.insert("sheet".into(), json!(s));
                    }
                    if table == "requirement" {
                        let id = text(&row["id"]);
                        self.campaign_cells(&mut row, &id)?;
                    }
                    rows.push(row);
                }
            }
            total += count;
        }
        Ok(Page {
            rows,
            total,
            limit,
            offset,
        })
    }
    fn log(
        &self,
        action: &str,
        table: &str,
        id: &str,
        field: Option<&str>,
        old: Option<&Value>,
        new: Option<&Value>,
    ) -> Result<()> {
        self.db.execute(
            "INSERT INTO event(at,author,action,tbl,id,field,old,new) VALUES (?,?,?,?,?,?,?,?)",
            params![
                now(),
                self.author,
                action,
                table,
                id,
                field,
                old.filter(|v| !v.is_null())
                    .map(|v| py_json(v, false, true)),
                new.filter(|v| !v.is_null())
                    .map(|v| py_json(v, false, true))
            ],
        )?;
        Ok(())
    }
    fn check_status(&self, status: &Value) -> Result<()> {
        let s = status
            .as_str()
            .ok_or_else(|| Error::Invalid("status must be text".into()))?;
        if !self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM status_vocab WHERE status=?)",
            [s],
            |r| r.get::<_, bool>(0),
        )? {
            return Err(Error::Invalid(format!(
                "status {s:?} not in vocabulary; add it with `tracker vocab add`"
            )));
        }
        Ok(())
    }
    pub fn set(&self, id: &str, fields: &Fields, expect: Option<&Fields>) -> Result<()> {
        self.set_changed(id, fields, expect).map(|_| ())
    }
    pub fn set_changed(
        &self,
        id: &str,
        fields: &Fields,
        expect: Option<&Fields>,
    ) -> Result<Fields> {
        let tx = self.tx()?;
        let (table, key, cols) = self.locate(id)?;
        let mut changed = Fields::new();
        for (name, new) in fields {
            let f = field(name);
            let (t, k) = if table == "requirement" && CAMP_COLS.contains(&f.as_str()) {
                if f == "campaign_state"
                    && !new.is_null()
                    && !CAMP_STATES.contains(&new.as_str().unwrap_or(""))
                {
                    return Err(Error::Invalid("invalid campaign state".into()));
                }
                if self.row_at("requirement", "id", id)?["sheet"] != "QREAM" {
                    return Err(Error::Invalid(
                        "campaign fields exist only on QREAM rows".into(),
                    ));
                }
                self.db
                    .execute("INSERT OR IGNORE INTO campaign(id) VALUES (?)", [id])?;
                ("campaign", "id")
            } else if cols.contains(&f) {
                if f == key {
                    return Err(Error::Invalid("the ID column cannot be changed".into()));
                }
                if f == "status" && table == "requirement" {
                    self.check_status(new)?;
                }
                (table.as_str(), key.as_str())
            } else {
                return Err(Error::Invalid(format!("{table} has no field {name:?}")));
            };
            let old = self
                .row_at(t, k, id)?
                .get(&f)
                .cloned()
                .ok_or_else(|| Error::Invalid(format!("missing field {f}")))?;
            if let Some(e) = expect.and_then(|e| e.get(name))
                && old != *e
                && (old.is_null() || e.is_null() || text(&old) != text(e))
            {
                return Err(Error::Conflict {
                    id: id.into(),
                    field: f,
                    expected: py_json(e, false, false),
                    found: py_json(&old, false, false),
                });
            }
            let mut new = new.clone();
            if f == "age"
                && let Some(s) = new.as_str()
                && let Ok(i) = s.parse::<u64>()
            {
                new = json!(i);
            }
            if old == new {
                continue;
            }
            // QREAM controller writes are constrained here too, so `set` cannot
            // bypass the engine's status transition rules. CLI agents retain the
            // Python vocabulary behavior; non-author verification is the closing
            // engine task's responsibility, never a controller transition.
            if f == "status" && table == "requirement" && self.author == "qream-controller" {
                let previous = old.as_str().unwrap_or("");
                let target = new.as_str().unwrap_or("");
                if target == "implemented" && ["implemented", "verified"].contains(&previous) {
                    continue;
                }
                let allowed = match target {
                    "in build" => OPEN.contains(&previous),
                    "implemented" => true,
                    _ => false,
                };
                if !allowed {
                    return Err(Error::Invalid(format!(
                        "controller cannot move {previous:?} to {target:?}"
                    )));
                }
            }
            self.db.execute(
                &format!(
                    "UPDATE {} SET {}=? WHERE {}=?",
                    quote(t),
                    quote(&f),
                    quote(k)
                ),
                params![sql_value(&new)?, id],
            )?;
            self.log("set", t, id, Some(&f), Some(&old), Some(&new))?;
            changed.insert(f, json!({"old":old,"new":new}));
        }
        tx.commit()?;
        Ok(changed)
    }
    pub fn status(&self, id: &str, status: &str) -> Result<()> {
        let fields = [("status".into(), json!(status))].into_iter().collect();
        self.set(id, &fields, None)
    }
    fn insert(&self, table: &str, values: &Fields) -> Result<()> {
        let cols = values
            .keys()
            .map(|k| quote(k))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "INSERT INTO {}({cols}) VALUES ({})",
            quote(table),
            vec!["?"; values.len()].join(",")
        );
        let args = values.values().map(sql_value).collect::<Result<Vec<_>>>()?;
        self.db.execute(&sql, params_from_iter(args))?;
        Ok(())
    }
    pub fn add(&self, tab: &str, id: &str, fields: &Fields) -> Result<String> {
        let tx = self.tx()?;
        let kind: String = self
            .db
            .query_row("SELECT kind FROM sheet WHERE name=?", [tab], |r| r.get(0))
            .map_err(|_| Error::Invalid(format!("no tab {tab:?}")))?;
        let mut values: Fields = fields.iter().map(|(k, v)| (field(k), v.clone())).collect();
        if kind == "requirement" {
            if id.is_empty() {
                return Err(Error::Invalid("a new requirement needs --id".into()));
            }
            values.entry("tab").or_insert(json!(tab));
            if let Some(s) = values.get("status") {
                self.check_status(s)?;
            }
            let mut camp = Fields::new();
            for k in CAMP_COLS {
                if let Some(v) = values.remove(*k) {
                    camp.insert((*k).into(), v);
                }
            }
            for k in values.keys() {
                if !REQ_COLS.contains(&k.as_str()) {
                    return Err(Error::Invalid(format!("unknown field {k}")));
                }
            }
            let pos: i64 = self.db.query_row(
                "SELECT coalesce(max(position),1)+1 FROM requirement WHERE sheet=?",
                [tab],
                |r| r.get(0),
            )?;
            values.insert("id".into(), json!(id));
            values.insert("sheet".into(), json!(tab));
            values.insert("position".into(), json!(pos));
            self.insert("requirement", &values)?;
            if tab == "QREAM" || !camp.is_empty() {
                if tab != "QREAM" {
                    return Err(Error::Invalid(
                        "campaign fields exist only on QREAM rows".into(),
                    ));
                }
                camp.insert("id".into(), json!(id));
                self.insert("campaign", &camp)?;
            }
            let mut logged = values;
            logged.extend(camp);
            self.log("add", "requirement", id, None, None, Some(&json!(logged)))?;
        } else {
            let b = self
                .objects(
                    "SELECT tbl,cols FROM block WHERE sheet=? ORDER BY header_row DESC LIMIT 1",
                    &[SqlValue::Text(tab.into())],
                )?
                .into_iter()
                .next()
                .ok_or_else(|| Error::Invalid("register has no block".into()))?;
            let cols: Vec<String> = serde_json::from_str(&text(&b["cols"]))?;
            let key = cols
                .first()
                .ok_or_else(|| Error::Invalid("empty register block".into()))?;
            if !id.is_empty() {
                values.insert(key.clone(), json!(id));
            }
            for k in values.keys() {
                if !cols.contains(k) {
                    return Err(Error::Invalid(format!("unknown field {k}")));
                }
            }
            let table = text(&b["tbl"]);
            let pos: i64 = self.db.query_row(
                &format!("SELECT coalesce(max(_pos),0)+1 FROM {}", quote(&table)),
                [],
                |r| r.get(0),
            )?;
            values.insert("_pos".into(), json!(pos));
            self.insert(&table, &values)?;
            let rid = values.get(key).map(text).unwrap_or_default();
            self.log("add", &table, &rid, None, None, Some(&json!(values)))?;
            tx.commit()?;
            return Ok(rid);
        }
        tx.commit()?;
        Ok(id.into())
    }
    pub fn evidence(&self, id: &str, path: &Path, note: &str) -> Result<()> {
        self.evidence_digest(id, path, Some(note)).map(|_| ())
    }
    pub fn evidence_digest(
        &self,
        id: &str,
        path: &Path,
        note: Option<&str>,
    ) -> Result<Option<String>> {
        let path = if let Ok(rest) = path.strip_prefix("~") {
            std::env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .ok_or_else(|| Error::Invalid("HOME is unset".into()))?
                .join(rest)
        } else {
            path.into()
        };
        let resolved = path.canonicalize().map_err(|e| Error::io(&path, e))?;
        let digest = if resolved.is_file() {
            Some(
                Sha256::digest(std::fs::read(&resolved).map_err(|e| Error::io(&resolved, e))?)
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>(),
            )
        } else {
            None
        };
        let tx = self.tx()?;
        self.locate(id)?;
        self.db.execute(
            "INSERT INTO evidence(id,path,sha256,note,author,at) VALUES (?,?,?,?,?,?)",
            params![
                id,
                resolved.to_string_lossy(),
                digest,
                note,
                self.author,
                now()
            ],
        )?;
        let mut record = Fields::new();
        record.insert("path".into(), json!(resolved));
        record.insert("sha256".into(), json!(digest));
        record.insert("note".into(), json!(note));
        self.log("evidence", "evidence", id, None, None, Some(&json!(record)))?;
        tx.commit()?;
        Ok(digest)
    }
    pub fn links(&self, id: &str) -> Result<Vec<Assignment>> {
        self.locate(id)?;
        if !self.has("assignment")? {
            return Ok(Vec::new());
        }
        self.objects(
            "SELECT * FROM assignment WHERE row_id=? ORDER BY seq DESC",
            &[SqlValue::Text(id.into())],
        )?
        .into_iter()
        .map(|r| Ok(serde_json::from_value(json!(r))?))
        .collect()
    }
    pub fn assignments(&self, campaign: &str) -> Result<Vec<Assignment>> {
        if campaign.is_empty() {
            return Err(Error::Invalid("campaign must be nonempty".into()));
        }
        if !self.has("assignment")? {
            return Ok(Vec::new());
        }
        self.objects(
            "SELECT * FROM assignment WHERE campaign=? ORDER BY seq DESC",
            &[SqlValue::Text(campaign.into())],
        )?
        .into_iter()
        .map(|r| Ok(serde_json::from_value(json!(r))?))
        .collect()
    }
    fn assignment_schema(&self) -> Result<()> {
        self.db.execute_batch("CREATE TABLE IF NOT EXISTS assignment(seq INTEGER PRIMARY KEY AUTOINCREMENT,campaign TEXT NOT NULL,wave INTEGER NOT NULL,task TEXT NOT NULL,directive TEXT NOT NULL,row_id TEXT NOT NULL,part INTEGER NOT NULL CHECK(part IN (0,1)),state TEXT NOT NULL CHECK(state IN ('planned','in build','blocked','implemented','verified')),at TEXT NOT NULL,author TEXT NOT NULL,UNIQUE(campaign,task,row_id)); CREATE INDEX IF NOT EXISTS assignment_row ON assignment(row_id,seq);")?;
        Ok(())
    }
    pub fn assign(&self, id: &str, a: &Assignment) -> Result<()> {
        self.assign_record(id, a).map(|_| ())
    }
    pub fn assign_record(&self, id: &str, a: &Assignment) -> Result<Assignment> {
        if [&a.campaign, &a.task, &a.directive]
            .iter()
            .any(|s| s.trim().is_empty())
            || a.wave < 1
            || ![0, 1].contains(&a.part)
            || !ASSIGN_STATES.contains(&a.state.as_str())
        {
            return Err(Error::Invalid("invalid assignment".into()));
        }
        let tx = self.tx()?;
        self.locate(id)?;
        self.assignment_schema()?;
        let old = self
            .objects(
                "SELECT * FROM assignment WHERE campaign=? AND task=? AND row_id=?",
                &[
                    SqlValue::Text(a.campaign.clone()),
                    SqlValue::Text(a.task.clone()),
                    SqlValue::Text(id.into()),
                ],
            )?
            .into_iter()
            .next();
        self.db.execute(
            "DELETE FROM assignment WHERE campaign=? AND task=? AND row_id=?",
            params![a.campaign, a.task, id],
        )?;
        self.db.execute("INSERT INTO assignment(campaign,wave,task,directive,row_id,part,state,at,author) VALUES (?,?,?,?,?,?,?,?,?)",params![a.campaign,a.wave,a.task,a.directive,id,a.part,a.state,now(),self.author])?;
        let record = self.row_at(
            "assignment",
            "seq",
            &self.db.last_insert_rowid().to_string(),
        )?;
        self.log(
            "assign",
            "assignment",
            id,
            None,
            old.as_ref().map(|r| json!(r)).as_ref(),
            Some(&json!(record)),
        )?;
        let result = serde_json::from_value(json!(record))?;
        tx.commit()?;
        Ok(result)
    }
    pub fn unassign(&self, id: &str) -> Result<()> {
        self.unassign_filtered(id, None, None).map(|_| ())
    }
    pub fn unassign_filtered(
        &self,
        id: &str,
        campaign: Option<&str>,
        task: Option<&str>,
    ) -> Result<Vec<Assignment>> {
        let tx = self.tx()?;
        self.locate(id)?;
        self.assignment_schema()?;
        let mut sql = "SELECT * FROM assignment WHERE row_id=?".to_string();
        let mut args = vec![SqlValue::Text(id.into())];
        if let Some(c) = campaign {
            if c.is_empty() {
                return Err(Error::Invalid("campaign must be nonempty".into()));
            }
            sql.push_str(" AND campaign=?");
            args.push(SqlValue::Text(c.into()));
        }
        if let Some(t) = task {
            sql.push_str(" AND task=?");
            args.push(SqlValue::Text(t.into()));
        }
        sql.push_str(" ORDER BY seq DESC");
        let old = self.objects(&sql, &args)?;
        for r in &old {
            self.db.execute(
                "DELETE FROM assignment WHERE seq=?",
                [sql_value(&r["seq"])?],
            )?;
            self.log("unassign", "assignment", id, None, Some(&json!(r)), None)?;
        }
        let records = old
            .into_iter()
            .map(|r| Ok(serde_json::from_value(json!(r))?))
            .collect::<Result<Vec<_>>>()?;
        tx.commit()?;
        Ok(records)
    }
    pub fn usage(&self, u: &Usage) -> Result<()> {
        self.usage_record(u).map(|_| ())
    }
    pub fn usage_record(&self, u: &Usage) -> Result<i64> {
        for k in u.keys() {
            if !USAGE_FIELDS.contains(&k.as_str()) {
                return Err(Error::Invalid(format!("unknown usage field {k}")));
            }
        }
        for k in ["campaign", "role", "unit"] {
            if u.get(k).is_none_or(|v| v.is_null() || text(v).is_empty()) {
                return Err(Error::Invalid(format!("usage needs {k}")));
            }
        }
        let tx = self.tx()?;
        self.db.execute_batch(&format!("CREATE TABLE IF NOT EXISTS usage(seq INTEGER PRIMARY KEY,at TEXT NOT NULL,author TEXT NOT NULL,{})",USAGE_FIELDS.join(",")))?;
        let mut values = Fields::new();
        values.insert("at".into(), json!(now()));
        values.insert("author".into(), json!(self.author));
        values.extend(u.clone());
        self.insert("usage", &values)?;
        let seq = self.db.last_insert_rowid();
        self.log(
            "usage",
            "usage",
            &text(&u["unit"]),
            None,
            None,
            Some(&json!(u)),
        )?;
        tx.commit()?;
        Ok(seq)
    }
    pub fn duration(&self, d: &Duration) -> Result<()> {
        if !["build", "test", "check", "install", "task"].contains(&d.kind.as_str())
            || !d.seconds.is_finite()
            || d.seconds < 0.0
        {
            return Err(Error::Invalid("invalid duration".into()));
        }
        let values = serde_json::to_value(d)?
            .as_object()
            .cloned()
            .ok_or_else(|| Error::Invalid("duration must be an object".into()))?;
        let tx = self.tx()?;
        self.insert("duration", &values)?;
        self.log(
            "duration",
            "duration",
            &self.db.last_insert_rowid().to_string(),
            None,
            None,
            Some(&json!(values)),
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn fault(&self, f: &Fault) -> Result<String> {
        let values = serde_json::to_value(f)?
            .as_object()
            .cloned()
            .ok_or_else(|| Error::Invalid("fault must be an object".into()))?;
        let tx = self.tx()?;
        self.insert("fault", &values)?;
        let id = self.db.last_insert_rowid().to_string();
        self.log("fault", "fault", &id, None, None, Some(&json!(values)))?;
        tx.commit()?;
        Ok(id)
    }
    pub fn events_since(&self, seq: u64) -> Result<Vec<Event>> {
        let mut events = self.objects(
            "SELECT * FROM event WHERE seq>? ORDER BY seq",
            &[SqlValue::Integer(i64::try_from(seq).map_err(|_| {
                Error::Invalid("event cursor out of range".into())
            })?)],
        )?;
        for e in &mut events {
            for k in ["old", "new"] {
                if let Some(s) = e[k].as_str() {
                    let value = serde_json::from_str(s)?;
                    e.insert(k.into(), value);
                }
            }
        }
        Ok(events)
    }
    pub fn add_vocab(&self, status: &str, meaning: &str) -> Result<()> {
        let tx = self.tx()?;
        self.db.execute("INSERT INTO status_vocab(status,meaning,position) VALUES (?,?,(SELECT coalesce(max(position),0)+1 FROM status_vocab))",params![status,meaning])?;
        self.log(
            "vocab-add",
            "status_vocab",
            status,
            None,
            None,
            Some(&json!(meaning)),
        )?;
        tx.commit()?;
        Ok(())
    }
    pub(crate) fn usage_query(&self, campaign: Option<&str>) -> Result<Query> {
        if !self.has("usage")? {
            return Ok((vec!["usage".into()], Vec::new()));
        }
        let sql = "SELECT seq,at,campaign,role,unit,run,tokens_in,tokens_cached,tokens_out,cpu_s,wall_s,outcome FROM usage";
        self.query_params(
            &format!(
                "{sql}{} ORDER BY seq",
                if campaign.is_some() {
                    " WHERE campaign=?"
                } else {
                    ""
                }
            ),
            &campaign
                .map(|c| vec![SqlValue::Text(c.into())])
                .unwrap_or_default(),
        )
    }
}
