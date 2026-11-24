use crate::types::*;
use crate::{Error, Result, Store};
use rusqlite::types::Value as SqlValue;
use rust_xlsxwriter::{
    ConditionalFormatCell, ConditionalFormatCellRule, ConditionalFormatFormula, ExcelDateTime,
    Format, Formula, Workbook, Worksheet,
};
use serde_json::{Value, json};
use std::path::Path;

impl Store {
    pub fn export(
        &self,
        kind: Export,
        out: &Path,
        tab: Option<&str>,
        sql: Option<&str>,
    ) -> Result<()> {
        self.export_tabs(
            kind,
            out,
            &tab.into_iter().map(String::from).collect::<Vec<_>>(),
            sql,
        )
    }
    pub fn export_tabs(
        &self,
        kind: Export,
        out: &Path,
        tabs: &[String],
        sql: Option<&str>,
    ) -> Result<()> {
        if matches!(kind, Export::Parquet) {
            return Err(Error::Unimplemented(
                "Parquet needs an analysis writer not present in the contracted offline dependencies; SQLite remains the record",
            ));
        }
        // One read transaction prevents a concurrent edit from tearing a workbook.
        let tx = self.db.unchecked_transaction()?;
        if matches!(kind, Export::Xlsx) && sql.is_none() {
            self.workbook(out, tabs)?;
        } else {
            let default = format!(
                "SELECT r.*,c.campaign,c.campaign_state,c.campaign_evidence,c.campaign_remaining,c.campaign_updated_at FROM requirement r LEFT JOIN campaign c USING(id){} ORDER BY r.sheet,r.position",
                if tabs.is_empty() {
                    String::new()
                } else {
                    format!(
                        " WHERE r.sheet IN ({})",
                        tabs.iter()
                            .map(|t| format!("'{}'", t.replace('\'', "''")))
                            .collect::<Vec<_>>()
                            .join(",")
                    )
                }
            );
            let (cols, mut rows) = self.query(sql.unwrap_or(&default))?;
            if let Some(i) = cols.iter().position(|c| c == "status") {
                rows.sort_by_key(|r| !active(r.get(i)));
            }
            if matches!(kind, Export::Xlsx) {
                let mut wb = Workbook::new();
                let ws = wb.add_worksheet();
                for (c, s) in cols.iter().enumerate() {
                    ws.write_string(0, c as u16, s)?;
                }
                for (r, row) in rows.iter().enumerate() {
                    for (c, v) in row.iter().enumerate() {
                        write_cell(ws, (r + 1) as u32, c as u16, v, None)?;
                    }
                }
                wb.save(out)?;
            } else {
                let file = std::fs::File::create(out).map_err(|e| Error::io(out, e))?;
                let mut writer = csv::WriterBuilder::new()
                    .delimiter(if matches!(kind, Export::Tsv) {
                        b'\t'
                    } else {
                        b','
                    })
                    .terminator(csv::Terminator::CRLF)
                    .from_writer(file);
                writer.write_record(&cols)?;
                for r in rows {
                    writer.write_record(r.iter().map(text))?;
                }
                writer.flush().map_err(|e| Error::io(out, e))?;
            }
        }
        tx.commit()?;
        Ok(())
    }
    fn workbook(&self, out: &Path, tabs: &[String]) -> Result<()> {
        let mut wb = Workbook::new();
        for sheet in self.objects("SELECT * FROM sheet ORDER BY position", &[])? {
            let name = text(&sheet["name"]);
            if !tabs.is_empty() && !tabs.contains(&name) {
                continue;
            }
            let requirement = sheet["kind"] == "requirement";
            let mut items: Vec<(i64, Vec<Vec<Value>>)> = Vec::new();
            for layout in self.objects(
                "SELECT row_no,cells FROM layout_row WHERE sheet=?",
                &[SqlValue::Text(name.clone())],
            )? {
                items.push((
                    layout["row_no"]
                        .as_i64()
                        .ok_or_else(|| Error::Invalid("invalid layout position".into()))?,
                    vec![serde_json::from_str(&text(&layout["cells"]))?],
                ));
            }
            if requirement {
                let has_c = sheet["max_col"].as_u64().unwrap_or(0) > REQ_HEADERS.len() as u64;
                let mut headers = REQ_HEADERS.iter().map(|s| json!(s)).collect::<Vec<_>>();
                let mut cols = REQ_COLS.to_vec();
                if has_c {
                    headers.extend(CAMP_HEADERS.iter().map(|s| json!(s)));
                    cols.extend(CAMP_COLS);
                }
                items.push((1, vec![headers]));
                let selection = cols
                    .iter()
                    .map(|c| {
                        format!(
                            "{}.{}",
                            if CAMP_COLS.contains(c) { "c" } else { "r" },
                            quote(c)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(",");
                let (_,mut rows)=self.query_params(&format!("SELECT {selection} FROM requirement r LEFT JOIN campaign c USING(id) WHERE r.sheet=? ORDER BY r.position"),&[SqlValue::Text(name.clone())])?;
                let status = cols.iter().position(|c| *c == "status");
                if let Some(i) = status {
                    rows.sort_by_key(|r| !active(r.get(i)));
                }
                // Retain layout slots while inserting the stable active-first sequence.
                let positions = self
                    .query_params(
                        "SELECT position FROM requirement WHERE sheet=? ORDER BY position",
                        &[SqlValue::Text(name.clone())],
                    )?
                    .1;
                for (pos, row) in positions.into_iter().zip(rows) {
                    items.push((
                        pos[0]
                            .as_i64()
                            .ok_or_else(|| Error::Invalid("invalid requirement position".into()))?,
                        vec![row],
                    ));
                }
            } else {
                for b in self.objects(
                    "SELECT * FROM block WHERE sheet=?",
                    &[SqlValue::Text(name.clone())],
                )? {
                    let headers: Vec<Value> = serde_json::from_str(&text(&b["headers"]))?;
                    let cols: Vec<String> = serde_json::from_str(&text(&b["cols"]))?;
                    let (_, mut rows) = self.query(&format!(
                        "SELECT {} FROM {} ORDER BY _pos",
                        cols.iter().map(|c| quote(c)).collect::<Vec<_>>().join(","),
                        quote(&text(&b["tbl"]))
                    ))?;
                    if let Some(i) = cols.iter().position(|c| c == "status") {
                        rows.sort_by_key(|r| !active(r.get(i)));
                    }
                    rows.insert(0, headers);
                    items.push((
                        b["header_row"]
                            .as_i64()
                            .ok_or_else(|| Error::Invalid("invalid block position".into()))?,
                        rows,
                    ));
                }
            }
            items.sort_by_key(|i| i.0);
            let rows = items.into_iter().flat_map(|i| i.1).collect::<Vec<_>>();
            let presentation: Value = serde_json::from_str(&text(&sheet["presentation"]))
                .or_else(|_| serde_json::from_str("{}"))?;
            let ws = wb.add_worksheet();
            ws.set_name(&name)?;
            let mut header_index = 0;
            for (r, row) in rows.iter().enumerate() {
                let is_header = row.first().and_then(Value::as_str).is_some_and(|s| {
                    ["ID", "CHARTER", "UNIT", "OUTCOME ID", "CANONICAL", "TOOL"].contains(&s)
                });
                let style = if is_header {
                    let s = presentation
                        .get("header_rows")
                        .and_then(Value::as_array)
                        .and_then(|a| a.get(header_index));
                    header_index += 1;
                    s
                } else {
                    None
                };
                let mut fmt = Format::new();
                if let Some(s) = style {
                    if s["bold"].as_bool() == Some(true) {
                        fmt = fmt.set_bold();
                    }
                    if let Some(c) = s["font"].as_str() {
                        fmt = fmt.set_font_color(color(c));
                    }
                    if let Some(c) = s["fill"].as_str() {
                        fmt = fmt.set_background_color(color(c));
                    }
                    if let Some(h) = s["height"].as_f64() {
                        ws.set_row_height(r as u32, h)?;
                    }
                }
                for (c, v) in row.iter().enumerate() {
                    let date = if requirement && c == 12 && r > 0 {
                        v.as_str()
                            .filter(|s| s.len() == 19 && s.as_bytes()[10] == b' ')
                    } else {
                        None
                    };
                    if let Some(s) = date {
                        let dt = ExcelDateTime::parse_from_str(&s.replace(' ', "T"))?;
                        let nf = presentation["formats"][(c + 1).to_string()]
                            .as_str()
                            .unwrap_or("yyyy-mm-dd hh:mm:ss");
                        ws.write_datetime_with_format(
                            r as u32,
                            c as u16,
                            &dt,
                            &Format::new().set_num_format(nf),
                        )?;
                    } else {
                        write_cell(ws, r as u32, c as u16, v, style.map(|_| &fmt))?;
                    }
                }
            }
            if let Some(widths) = presentation["widths"].as_object() {
                for (col, w) in widths {
                    if let Some(w) = w.as_f64() {
                        ws.set_column_width(column(col)?, w)?;
                    }
                }
            }
            if let Some(freeze) = presentation["freeze"].as_str() {
                let (r, c) = cell(freeze)?;
                ws.set_freeze_panes(r, c)?;
            }
            conditional_formats(ws, &presentation, requirement, rows.len())?;
            if requirement && let Some(filter) = presentation["filter"].as_str() {
                let parts = filter.split(':').collect::<Vec<_>>();
                if parts.len() == 2 {
                    let (r, c) = cell(parts[0])?;
                    let (_, last_c) = cell(parts[1])?;
                    ws.autofilter(r, c, rows.len().saturating_sub(1) as u32, last_c)?;
                }
            }
        }
        wb.save(out)?;
        Ok(())
    }
}
fn color(s: &str) -> rust_xlsxwriter::Color {
    rust_xlsxwriter::Color::from(
        format!("#{}", s.get(s.len().saturating_sub(6)..).unwrap_or(s)).as_str(),
    )
}
fn column(s: &str) -> Result<u16> {
    let mut n = 0u32;
    for c in s.chars() {
        if !c.is_ascii_uppercase() {
            return Err(Error::Invalid(format!("invalid Excel column {s}")));
        }
        n = n * 26 + (c as u32 - 'A' as u32 + 1);
    }
    u16::try_from(n.saturating_sub(1))
        .map_err(|_| Error::Invalid("Excel column out of range".into()))
}
fn cell(s: &str) -> Result<(u32, u16)> {
    let split = s
        .find(|c: char| c.is_ascii_digit())
        .ok_or_else(|| Error::Invalid(format!("invalid Excel cell {s}")))?;
    let r = s[split..]
        .parse::<u32>()
        .map_err(|_| Error::Invalid(format!("invalid Excel cell {s}")))?;
    Ok((r.saturating_sub(1), column(&s[..split])?))
}
fn write_cell(
    ws: &mut Worksheet,
    r: u32,
    c: u16,
    v: &Value,
    format: Option<&Format>,
) -> Result<()> {
    match v {
        Value::Null => {
            if let Some(f) = format {
                ws.write_blank(r, c, f)?;
            }
        }
        Value::Bool(b) => {
            if let Some(f) = format {
                ws.write_boolean_with_format(r, c, *b, f)?;
            } else {
                ws.write_boolean(r, c, *b)?;
            }
        }
        Value::Number(n) => {
            let f = n
                .as_f64()
                .ok_or_else(|| Error::Invalid("invalid workbook number".into()))?;
            if let Some(fmt) = format {
                ws.write_number_with_format(r, c, f, fmt)?;
            } else {
                ws.write_number(r, c, f)?;
            }
        }
        Value::String(s) => {
            if let Some(f) = format {
                ws.write_string_with_format(r, c, s, f)?;
            } else {
                ws.write_string(r, c, s)?;
            }
        }
        _ => return Err(Error::Invalid("non-scalar workbook cell".into())),
    }
    Ok(())
}

fn conditional_formats(
    ws: &mut Worksheet,
    presentation: &Value,
    requirement: bool,
    last: usize,
) -> Result<()> {
    let Some(formats) = presentation["cf"].as_array() else {
        return Ok(());
    };
    for cf in formats {
        let range = cf["sqref"]
            .as_str()
            .ok_or_else(|| Error::Invalid("conditional format lacks range".into()))?;
        let ranges = range
            .split_whitespace()
            .map(|s| {
                let parts = s.split(':').collect::<Vec<_>>();
                let (r, c) = cell(parts[0])?;
                let (end_r, end_c) = if parts.len() == 2 {
                    cell(parts[1])?
                } else {
                    (r, c)
                };
                Ok((
                    r,
                    c,
                    if requirement && parts.len() == 2 {
                        last.saturating_sub(1) as u32
                    } else {
                        end_r
                    },
                    end_c,
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        for rule in cf["rules"]
            .as_array()
            .ok_or_else(|| Error::Invalid("conditional format lacks rules".into()))?
        {
            let mut format = Format::new();
            if let Some(s) = rule["fill"].as_str().or_else(|| rule["bg"].as_str()) {
                format = format.set_background_color(color(s));
            }
            if let Some(s) = rule["font"].as_str() {
                format = format.set_font_color(color(s));
            }
            if rule["bold"].as_bool() == Some(true) {
                format = format.set_bold();
            }
            let formulas = rule["formula"]
                .as_array()
                .ok_or_else(|| Error::Invalid("conditional format lacks formula".into()))?;
            let first = formulas
                .first()
                .and_then(Value::as_str)
                .ok_or_else(|| Error::Invalid("conditional format has no formula".into()))?;
            let stop = rule["stop"].as_bool().unwrap_or(false);
            match rule["type"].as_str() {
                Some("expression") => {
                    let cond = ConditionalFormatFormula::new()
                        .set_rule(first)
                        .set_format(format)
                        .set_stop_if_true(stop);
                    for (r, c, er, ec) in &ranges {
                        ws.add_conditional_format(*r, *c, *er, *ec, &cond)?;
                    }
                }
                Some("cellIs") => {
                    let a = Formula::new(first);
                    let b = || {
                        formulas
                            .get(1)
                            .and_then(Value::as_str)
                            .map(Formula::new)
                            .ok_or_else(|| {
                                Error::Invalid("conditional format missing second formula".into())
                            })
                    };
                    let op = match rule["operator"].as_str() {
                        Some("equal") => ConditionalFormatCellRule::EqualTo(a),
                        Some("notEqual") => ConditionalFormatCellRule::NotEqualTo(a),
                        Some("greaterThan") => ConditionalFormatCellRule::GreaterThan(a),
                        Some("greaterThanOrEqual") => {
                            ConditionalFormatCellRule::GreaterThanOrEqualTo(a)
                        }
                        Some("lessThan") => ConditionalFormatCellRule::LessThan(a),
                        Some("lessThanOrEqual") => ConditionalFormatCellRule::LessThanOrEqualTo(a),
                        Some("between") => ConditionalFormatCellRule::Between(a, b()?),
                        Some("notBetween") => ConditionalFormatCellRule::NotBetween(a, b()?),
                        _ => {
                            return Err(Error::Invalid(
                                "unsupported cell conditional operator".into(),
                            ));
                        }
                    };
                    let cond = ConditionalFormatCell::new()
                        .set_rule(op)
                        .set_format(format)
                        .set_stop_if_true(stop);
                    for (r, c, er, ec) in &ranges {
                        ws.add_conditional_format(*r, *c, *er, *ec, &cond)?;
                    }
                }
                _ => return Err(Error::Invalid("unsupported conditional format type".into())),
            }
        }
    }
    Ok(())
}
