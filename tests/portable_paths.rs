//! Inspect path selection in child test processes, without opening runtime state.
use std::{path::PathBuf, process::Command};

#[test]
fn path_probe_child() {
    if std::env::var_os("QREAM_PATH_TEST_CHILD").is_none() {
        return;
    }
    let paths = qream::engine::daemon::Paths::from_env().expect("configured paths");
    let account = qream::engine::daemon::operator_home().expect("account home");
    assert_ne!(
        account,
        PathBuf::from(std::env::var_os("HOME").expect("fixture home"))
    );
    assert_eq!(
        paths.store,
        PathBuf::from(std::env::var_os("EXPECTED_STORE").expect("expected store"))
    );
}

#[test]
fn portable_defaults_and_live_state_guard() {
    let temp = tempfile::tempdir().expect("isolated fixture");
    let root = temp.path();
    std::fs::create_dir_all(root.join("home")).expect("fixture home");
    std::fs::create_dir_all(root.join("runtime")).expect("fixture runtime");
    let run = |test: bool, unit: &str, home: PathBuf, store: Option<PathBuf>| {
        let mut command = Command::new(std::env::current_exe().expect("test binary"));
        command
            .args(["--exact", "path_probe_child", "--nocapture"])
            .env("QREAM_PATH_TEST_CHILD", "1")
            .env("HOME", root.join("home"))
            .env("XDG_DATA_HOME", root.join("data"))
            .env("XDG_RUNTIME_DIR", root.join("runtime"))
            .env("QREAM_HOME", home)
            .env("QREAM_SOCKET", root.join("private.sock"))
            .env("QREAM_UNIT", unit)
            .env(
                "EXPECTED_STORE",
                store
                    .clone()
                    .unwrap_or(root.join("data/qream/tracker.sqlite")),
            )
            .env_remove("QREAM_TEST")
            .env_remove("QREAM_STORE");
        if test {
            command.env("QREAM_TEST", "1");
        }
        if let Some(store) = store {
            command.env("QREAM_STORE", store);
        }
        command.output().expect("child probe")
    };
    let normal = run(false, "private-unit", root.join("private"), None);
    assert!(
        normal.status.success(),
        "{}",
        String::from_utf8_lossy(&normal.stderr)
    );
    let explicit = run(
        false,
        "private-unit",
        root.join("private"),
        Some(root.join("custom.sqlite")),
    );
    assert!(
        explicit.status.success(),
        "{}",
        String::from_utf8_lossy(&explicit.stderr)
    );
    let default_unit = run(
        true,
        "qream-daemon",
        root.join("private"),
        Some(root.join("store.sqlite")),
    );
    assert_eq!(default_unit.status.code(), Some(64));
    let real = qream::engine::daemon::operator_home().expect("account home");
    let live_home = run(
        true,
        "private-unit",
        real.join(".local/share/qream"),
        Some(root.join("store.sqlite")),
    );
    assert_eq!(live_home.status.code(), Some(64));
    let private = run(
        true,
        "private-unit",
        root.join("private"),
        Some(root.join("store.sqlite")),
    );
    assert!(
        private.status.success(),
        "{}",
        String::from_utf8_lossy(&private.stderr)
    );
}

#[test]
fn relative_xdg_data_home_is_ignored() {
    let fixture = tempfile::tempdir().expect("isolated fixture");
    let expected = qream::engine::daemon::operator_home()
        .expect("account home")
        .join(".local/share/qream/tracker.sqlite");
    let result = Command::new(std::env::current_exe().expect("test binary"))
        .args(["--exact", "path_probe_child", "--nocapture"])
        .env("QREAM_PATH_TEST_CHILD", "1")
        .env("HOME", fixture.path())
        .env("XDG_DATA_HOME", "relative/data")
        .env("XDG_RUNTIME_DIR", fixture.path().join("runtime"))
        .env("EXPECTED_STORE", expected)
        .env_remove("QREAM_STORE")
        .env_remove("QREAM_TEST")
        .output()
        .expect("path probe");
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}
