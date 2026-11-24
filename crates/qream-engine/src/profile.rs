//! Campaign choices are resolved once when a wave is pinned, never into directives.
use crate::{Error, Result, storage};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Profile {
    pub kind: String,
    pub model: String,
    pub effort: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profiles {
    pub orchestrator: Profile,
    pub worker: Profile,
    #[serde(default)]
    pub waves: BTreeMap<u32, Profile>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PinnedWave {
    pub wave: u32,
    pub worker: Profile,
    pub orchestrator: Profile,
    pub args: Vec<String>,
}
pub fn args(kind: &str, model: &str, effort: &str) -> Result<Vec<String>> {
    let cwd = storage::io(Path::new("."), std::env::current_dir())?;
    args_for_cwd(kind, model, effort, &cwd)
}
/// The native trust setting must name the worker's actual absolute project directory.
pub fn args_for_cwd(kind: &str, model: &str, effort: &str, cwd: &Path) -> Result<Vec<String>> {
    let cwd = storage::io(cwd, std::fs::canonicalize(cwd))?;
    let trust = format!(
        "projects.{}.trust_level=\"trusted\"",
        serde_json::to_string(&cwd.to_string_lossy())?
    );
    if model.is_empty()
        || model.chars().any(char::is_whitespace)
        || effort.is_empty()
        || !effort.chars().all(|c| c.is_ascii_lowercase())
    {
        return Err(Error::Denied("invalid model or effort".into()));
    }
    let a = match kind {
        "codex" => vec![
            "--model".into(),
            model.into(),
            "-c".into(),
            format!("model_reasoning_effort=\"{effort}\""),
            "--sandbox".into(),
            "danger-full-access".into(),
            "--ask-for-approval".into(),
            "never".into(),
            "--no-alt-screen".into(),
            "-c".into(),
            trust,
        ],
        "claude" => vec![
            "--model".into(),
            model.into(),
            "--effort".into(),
            effort.into(),
            "--dangerously-skip-permissions".into(),
        ],
        "opencode" => vec!["--model".into(), model.into()],
        _ => return Err(Error::Denied("unknown harness kind".into())),
    };
    Ok(a)
}
pub fn get(root: &Path) -> Result<Profiles> {
    storage::read(&root.join("controller/profiles.json"))
}
pub fn set(root: &Path, profiles: &Profiles) -> Result<()> {
    args(
        &profiles.worker.kind,
        &profiles.worker.model,
        &profiles.worker.effort,
    )?;
    args(
        &profiles.orchestrator.kind,
        &profiles.orchestrator.model,
        &profiles.orchestrator.effort,
    )?;
    for p in profiles.waves.values() {
        args(&p.kind, &p.model, &p.effort)?;
    }
    let path = root.join("controller/profiles.json");
    let _guard = storage::lock(&root.join("controller/.profiles.lock"))?;
    if path.exists() {
        let previous: Profiles = storage::read(&path)?;
        storage::write(&path.with_extension("json.before"), &previous)?;
    }
    storage::write(&path, profiles)
}
pub fn pin(root: &Path, wave: u32) -> Result<PinnedWave> {
    let path = root.join(format!("controller/wave{wave}.profile.json"));
    let _guard = storage::lock(&root.join("controller/.profiles.lock"))?;
    if path.exists() {
        return storage::read(&path);
    }
    let profiles = get(root)?;
    let p = profiles
        .waves
        .get(&wave)
        .unwrap_or(&profiles.worker)
        .clone();
    let pinned = PinnedWave {
        wave,
        args: args(&p.kind, &p.model, &p.effort)?,
        worker: p,
        orchestrator: profiles.orchestrator,
    };
    storage::write(&path, &pinned)?;
    Ok(pinned)
}
/// Resolve profiles into the contracted wave manifest once, before release.
/// Repeated calls read the existing frozen manifest and do not apply later Settings changes.
pub fn pin_manifest(
    root: &Path,
    wave: u32,
    mut manifest: serde_json::Value,
) -> Result<serde_json::Value> {
    let pinned = pin(root, wave)?;
    let path = root.join(format!("controller/wave{wave}.manifest.json"));
    let _guard = storage::lock(&root.join("controller/.profiles.lock"))?;
    if path.exists() {
        return storage::read(&path);
    }
    let tasks = manifest["tasks"]
        .as_array_mut()
        .ok_or_else(|| Error::Denied("wave manifest needs tasks".into()))?;
    for task in tasks {
        task["kind"] = serde_json::json!(pinned.worker.kind);
        let cwd = task["cwd"]
            .as_str()
            .ok_or_else(|| Error::Denied("task cwd missing".into()))?;
        let cwd = if Path::new(cwd).is_absolute() {
            std::path::PathBuf::from(cwd)
        } else {
            root.join(cwd)
        };
        task["args"] = serde_json::json!(args_for_cwd(
            &pinned.worker.kind,
            &pinned.worker.model,
            &pinned.worker.effort,
            &cwd
        )?);
    }
    manifest["profiles"] = serde_json::to_value(&pinned)?;
    storage::write(&path, &manifest)?;
    Ok(manifest)
}
