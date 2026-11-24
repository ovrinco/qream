//! The daemon: owns the terminal layer, the engine and the socket; started on demand.
fn main() {
    if std::env::args().nth(1).as_deref() == Some("--version") {
        std::process::exit(qream::cli::run("qreamd"));
    }
    let identity = qream::build_identity();
    // Run from a staged release (`releases/<id>/bin/qreamd` beside its record), the build is the release id.
    let release = std::env::current_exe().ok().and_then(|exe| {
        let dir = exe.parent()?.parent()?;
        dir.join("release_record.json")
            .is_file()
            .then(|| dir.file_name()?.to_str().map(str::to_owned))
            .flatten()
    });
    let build = release.unwrap_or_else(|| {
        format!(
            "{}+{}",
            identity.version,
            identity.git_describe.unwrap_or_else(|| "unknown".into())
        )
    });
    if let Err(error) = qream::engine::daemon::server_main(build) {
        eprintln!(
            "{}",
            serde_json::json!({"error":{"code":error.code(),"message":error.to_string()}})
        );
        std::process::exit(2);
    }
}
