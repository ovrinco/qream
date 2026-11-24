//! Thin transient-unit front; formats and ownership live in the library.
use qream_engine::{Error, Result, job};
use std::path::Path;
fn run() -> Result<i32> {
    let a: Vec<_> = std::env::args().collect();
    if a.len() < 5 {
        return Err(Error::Denied(
            "monitor requires run|finish ROOT TASK NAME [EXIT]".into(),
        ));
    }
    match a[1].as_str() {
        "run" => job::run(Path::new(&a[2]), &a[3], &a[4]),
        "finish" => {
            let code = a.get(5).and_then(|s| s.parse().ok()).unwrap_or(128);
            job::finish(Path::new(&a[2]), &a[3], &a[4], code)?;
            Ok(0)
        }
        _ => Err(Error::Denied("unknown monitor action".into())),
    }
}
fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!(
                "{}",
                serde_json::json!({"ok":false,"error":{"code":"job","message":e.to_string()}})
            );
            std::process::exit(2);
        }
    }
}
