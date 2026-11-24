use crate::types::*;
use crate::{Error, Result, Store};
use clap::{Parser, Subcommand};
use rusqlite::types::Value as SqlValue;
use serde_json::{Value, json};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "tracker",
    about = "The System and Project Tracker record (SQLite)."
)]
struct Cli {
    #[arg(long)]
    db: Option<PathBuf>,
    #[arg(long)]
    author: Option<String>,
    #[arg(long)]
    time: bool,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Get {
        id: String,
    },
    Query {
        sql: Option<String>,
        #[arg(short = 'n', long = "named")]
        named: Option<String>,
        #[arg(long,default_value="table",value_parser=["table","json","tsv"])]
        format: String,
    },
    Set {
        id: String,
        #[arg(required=true,num_args=1..)]
        fields: Vec<String>,
        #[arg(long)]
        expect: Vec<String>,
    },
    Status {
        id: String,
        status: String,
    },
    Vocab {
        add: Option<String>,
        status: Option<String>,
        meaning: Option<String>,
    },
    Add {
        tab: String,
        #[arg(long)]
        id: Option<String>,
        fields: Vec<String>,
    },
    Evidence {
        id: String,
        path: PathBuf,
        #[arg(long)]
        note: Option<String>,
    },
    Export {
        #[arg(value_parser=["xlsx","csv","tsv","parquet"])]
        format: String,
        out: PathBuf,
        #[arg(long)]
        tab: Vec<String>,
        #[arg(long)]
        sql: Option<String>,
    },
    /// DuckDB SQL over the store (the skill's `tracker duck`).
    Duck {
        sql: String,
    },
    Log {
        id: Option<String>,
        #[arg(long, default_value_t = 30)]
        limit: usize,
    },
    Assign {
        id: String,
        #[arg(long)]
        campaign: String,
        #[arg(long)]
        task: String,
        #[arg(long)]
        wave: i64,
        #[arg(long)]
        directive: String,
        #[arg(long)]
        part: bool,
        #[arg(long, default_value = "planned")]
        state: String,
    },
    Unassign {
        id: String,
        #[arg(long)]
        campaign: String,
        #[arg(long)]
        task: Option<String>,
    },
    Links {
        id: String,
    },
    Assignments {
        #[arg(long)]
        campaign: String,
    },
    Usage {
        #[arg(value_parser=["add","list"])]
        action: String,
        fields: Vec<String>,
        #[arg(long)]
        campaign: Option<String>,
        #[arg(long,default_value="table",value_parser=["table","json","tsv"])]
        format: String,
    },
}
pub fn run_cli() -> std::process::ExitCode {
    let cli = Cli::parse();
    let start = std::time::Instant::now();
    let timed = cli.time;
    let result = run(cli);
    if timed {
        eprintln!("elapsed {:.1} ms", start.elapsed().as_secs_f64() * 1000.0);
    }
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!(
                "{}",
                json!({"error":{"code":e.code(),"message":e.to_string()}})
            );
            std::process::ExitCode::from(2)
        }
    }
}
fn pairs(items: Vec<String>) -> Result<Fields> {
    items
        .into_iter()
        .map(|s| {
            let (k, v) = s
                .split_once('=')
                .ok_or_else(|| Error::Invalid(format!("expected FIELD=VALUE, got {s:?}")))?;
            Ok((k.into(), if v.is_empty() { Value::Null } else { json!(v) }))
        })
        .collect()
}
fn print_json(value: &Value) {
    println!("{}", py_json(value, true, false));
}
fn table(cols: Vec<String>, rows: Vec<Vec<Value>>, fmt: &str) {
    if fmt == "json" {
        print_json(&json!(
            rows.into_iter()
                .map(|r| cols.iter().cloned().zip(r).collect::<Fields>())
                .collect::<Vec<_>>()
        ));
        return;
    }
    if fmt == "tsv" {
        println!("{}", cols.join("\t"));
        for row in rows {
            println!(
                "{}",
                row.iter()
                    .map(|v| text(v).replace(['\t', '\n'], " "))
                    .collect::<Vec<_>>()
                    .join("\t")
            );
        }
        return;
    }
    let widths = cols
        .iter()
        .enumerate()
        .map(|(i, c)| {
            rows.iter()
                .map(|r| text(&r[i]).chars().count())
                .chain(std::iter::once(c.chars().count()))
                .max()
                .unwrap_or(0)
                .min(60)
        })
        .collect::<Vec<_>>();
    let pad = |s: &str, w: usize| format!("{s}{}", " ".repeat(w.saturating_sub(s.chars().count())));
    println!(
        "{}",
        cols.iter()
            .zip(&widths)
            .map(|(c, w)| pad(c, *w))
            .collect::<Vec<_>>()
            .join("  ")
    );
    for row in rows {
        println!(
            "{}",
            row.iter()
                .zip(&widths)
                .map(|(v, w)| pad(
                    &text(v)
                        .replace('\n', " ")
                        .chars()
                        .take(60)
                        .collect::<String>(),
                    *w
                ))
                .collect::<Vec<_>>()
                .join("  ")
        );
    }
}
fn named(name: &str) -> Result<&'static str> {
    Ok(match name {
        "status-counts" => {
            "SELECT sheet,status,count(*) n FROM requirement GROUP BY 1,2 ORDER BY 1,3 DESC"
        }
        "qream-campaign" => {
            "SELECT r.id,r.status,c.campaign_state,c.campaign_remaining,c.campaign_updated_at FROM requirement r JOIN campaign c USING(id) WHERE c.campaign_state IS NOT NULL AND c.campaign_state NOT IN ('Context / not commissioned') ORDER BY r.position"
        }
        "open" => {
            "SELECT id,sheet,status,purpose FROM requirement WHERE status IN ('said','proposed','Planning','planned','in build','built wrong') ORDER BY sheet,position"
        }
        "outcome-members" => {
            "SELECT outcome_id,count(*) n,group_concat(id,' ') ids FROM requirement WHERE outcome_id IS NOT NULL GROUP BY 1 ORDER BY 1"
        }
        "recent-events" => {
            "SELECT seq,at,author,action,tbl,id,field,old,new FROM event ORDER BY seq DESC LIMIT 50"
        }
        "added-rows" => {
            "SELECT id,sheet,status,purpose FROM requirement WHERE id NOT IN (SELECT value FROM json_each((SELECT value FROM meta WHERE key='original_ids')))"
        }
        _ => return Err(Error::Invalid(format!("unknown named query {name:?}"))),
    })
}
fn run(cli: Cli) -> Result<()> {
    let db = cli
        .db
        .or_else(|| std::env::var_os("TRACKER_DB").map(PathBuf::from))
        .map(Ok::<PathBuf, Error>)
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME")
                .filter(|v| !v.is_empty())
                .ok_or_else(|| {
                    Error::Invalid("HOME or TRACKER_DB must select the tracker store".into())
                })?;
            let base = std::env::var_os("XDG_DATA_HOME")
                .filter(|v| !v.is_empty() && PathBuf::from(v).is_absolute())
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(home).join(".local/share"));
            Ok(base.join("qream/tracker.sqlite"))
        })?;
    let author = cli
        .author
        .or_else(|| {
            std::env::var("TRACKER_AUTHOR")
                .ok()
                .filter(|s| !s.is_empty())
        })
        .or_else(|| std::env::var("USER").ok())
        .unwrap_or_else(|| "unknown".into());
    let st = Store::open(&db, &author)?;
    match cli.command {
        Command::Get { id } => print_json(&json!(st.get(&id)?)),
        Command::Query {
            sql,
            named: n,
            format,
        } => {
            let sql = if let Some(n) = n {
                named(&n)?.into()
            } else {
                sql.ok_or_else(|| Error::Invalid("give SQL or -n NAME".into()))?
            };
            let (cols, rows) = st.query(&sql)?;
            table(cols, rows, &format);
        }
        Command::Set { id, fields, expect } => {
            let ch = st.set_changed(
                &id,
                &pairs(fields)?,
                if expect.is_empty() {
                    None
                } else {
                    Some(pairs(expect)?)
                }
                .as_ref(),
            )?;
            if ch.is_empty() {
                println!("unchanged");
            } else {
                println!("{}", py_json(&json!(ch), false, false));
            }
        }
        Command::Status { id, status } => {
            let old = st.get(&id)?.get("status").cloned().unwrap_or(Value::Null);
            st.status(&id, &status)?;
            if old == status {
                println!("unchanged");
            } else {
                println!(
                    "{id}: {} -> {status}",
                    if old.is_null() {
                        "None".into()
                    } else {
                        text(&old)
                    }
                );
            }
        }
        Command::Vocab {
            add,
            status,
            meaning,
        } => {
            if let Some(add) = add {
                if add != "add" {
                    return Err(Error::Invalid("vocab add STATUS MEANING".into()));
                }
                st.add_vocab(
                    &status.ok_or_else(|| Error::Invalid("missing status".into()))?,
                    &meaning.ok_or_else(|| Error::Invalid("missing meaning".into()))?,
                )?;
            }
            let (c,r)=st.query("SELECT status,meaning,(SELECT count(*) FROM requirement r WHERE r.status=v.status) rows FROM status_vocab v ORDER BY position")?;
            table(c, r, "table");
        }
        Command::Add { tab, id, fields } => println!(
            "{}",
            st.add(&tab, id.as_deref().unwrap_or(""), &pairs(fields)?)?
        ),
        Command::Evidence { id, path, note } => println!(
            "{}",
            st.evidence_digest(&id, &path, note.as_deref())?
                .unwrap_or_else(|| "attached".into())
        ),
        Command::Export {
            format,
            out,
            tab,
            sql,
        } => {
            let kind = match format.as_str() {
                "xlsx" => Export::Xlsx,
                "csv" => Export::Csv,
                "tsv" => Export::Tsv,
                _ => Export::Parquet,
            };
            st.export_tabs(kind, &out, &tab, sql.as_deref())?;
            println!("{}", out.display());
        }
        Command::Duck { .. } => {
            return Err(Error::Invalid(
                "DuckDB is not among the contracted offline dependencies; run the SQL with `tracker query` (SQLite remains the record)"
                    .into(),
            ));
        }
        Command::Log { id, limit } => {
            let mut sql = "SELECT seq,at,author,action,tbl,id,field,old,new FROM event".to_string();
            let mut args = Vec::new();
            if let Some(id) = id {
                sql.push_str(" WHERE id=?");
                args.push(SqlValue::Text(id));
            }
            sql.push_str(" ORDER BY seq DESC LIMIT ?");
            args.push(SqlValue::Integer(limit as i64));
            let (c, r) = st.query_params(&sql, &args)?;
            table(c, r, "table");
        }
        Command::Assign {
            id,
            campaign,
            task,
            wave,
            directive,
            part,
            state,
        } => {
            let a = Assignment {
                seq: 0,
                campaign,
                task,
                wave,
                directive,
                row_id: id.clone(),
                part: i64::from(part),
                state,
                at: String::new(),
                author: String::new(),
            };
            print_json(&serde_json::to_value(st.assign_record(&id, &a)?)?);
        }
        Command::Unassign { id, campaign, task } => print_json(&serde_json::to_value(
            st.unassign_filtered(&id, Some(&campaign), task.as_deref())?,
        )?),
        Command::Links { id } => print_json(&serde_json::to_value(st.links(&id)?)?),
        Command::Assignments { campaign } => {
            print_json(&serde_json::to_value(st.assignments(&campaign)?)?)
        }
        Command::Usage {
            action,
            fields,
            campaign,
            format,
        } => {
            if action == "add" {
                println!("{}", st.usage_record(&pairs(fields)?)?);
            } else {
                let (c, r) = st.usage_query(campaign.as_deref())?;
                table(c, r, &format);
            }
        }
    }
    Ok(())
}
