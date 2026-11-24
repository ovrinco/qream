//! Inspect configured paths without opening a database or starting a daemon.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let paths = qream::engine::daemon::Paths::from_env()?;
    println!(
        "{}",
        serde_json::json!({
            "home": paths.home, "socket": paths.socket, "store": paths.store,
            "operator_home": qream::engine::daemon::operator_home()?,
        })
    );
    Ok(())
}
