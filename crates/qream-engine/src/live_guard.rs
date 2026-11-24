//! Refuse test startup before opening any configured runtime state.
use crate::{
    Error, Result,
    daemon::{Paths, default_store, operator_home},
};
use std::{
    fs,
    path::{Component, Path, PathBuf},
};

/// Test paths are checked through existing ancestors, including symlink aliases.
/// Live campaign records are inspected read-only to identify their reserved unit names.
pub fn check(paths: &Paths) -> Result<()> {
    if std::env::var_os("QREAM_TEST").is_none_or(|v| v != "1") {
        return Ok(());
    }
    let resolve = |path: &Path| -> Result<PathBuf> {
        let absolute = if path.is_absolute() {
            path.to_owned()
        } else {
            std::env::current_dir()
                .map_err(|e| Error::io(path, e))?
                .join(path)
        };
        let mut pending = absolute;
        // Restart on a link so even dangling links and link/.. resolve in filesystem order.
        for _ in 0..64 {
            let mut resolved = PathBuf::new();
            let current = pending.clone();
            let components: Vec<_> = current.components().collect();
            let mut link = false;
            for (index, component) in components.iter().enumerate() {
                match component {
                    Component::ParentDir => {
                        resolved.pop();
                    }
                    Component::CurDir => {}
                    _ => resolved.push(component.as_os_str()),
                }
                match fs::symlink_metadata(&resolved) {
                    Ok(meta) if meta.file_type().is_symlink() => {
                        let target =
                            fs::read_link(&resolved).map_err(|e| Error::io(&resolved, e))?;
                        resolved.pop();
                        pending = if target.is_absolute() {
                            target
                        } else {
                            resolved.join(target)
                        };
                        for rest in &components[index + 1..] {
                            pending.push(rest.as_os_str());
                        }
                        link = true;
                        break;
                    }
                    Ok(_) => {
                        resolved =
                            fs::canonicalize(&resolved).map_err(|e| Error::io(&resolved, e))?;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(Error::io(&resolved, e)),
                }
            }
            if !link {
                return Ok(resolved);
            }
        }
        Err(Error::Denied(format!(
            "Test path has too many symbolic links: {}",
            path.display()
        )))
    };
    let home = std::env::var_os("HOME")
        .ok_or_else(|| Error::Denied("Test user home is unavailable".into()))?;
    let user_home = PathBuf::from(home);
    let live_home = user_home.join(".local/share/qream");
    // Changing HOME in a fixture must not make the workstation's real state writable.
    let mut live_homes = vec![live_home];
    let workstation_home = operator_home()?;
    if user_home != workstation_home {
        live_homes.push(workstation_home.join(".local/share/qream"));
    }
    let live_roots: Vec<_> = [user_home, workstation_home]
        .into_iter()
        .flat_map(|home| {
            [
                home.join(".local/share/qream"),
                home.join(".local/state/qream"),
                home.join(".local/lib/qream"),
            ]
        })
        .collect();
    let roots = live_roots
        .iter()
        .map(|p| resolve(p))
        .collect::<Result<Vec<_>>>()?;
    let store = resolve(&default_store()?)?;
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .ok_or_else(|| Error::Denied("Test runtime directory is unavailable".into()))?;
    let socket = resolve(&PathBuf::from(runtime).join("qream/engine.sock"))?;
    use std::os::unix::fs::MetadataExt;
    let uid = fs::metadata(&operator_home()?)
        .map_err(|e| Error::io("account home", e))?
        .uid();
    let workstation_socket = resolve(&PathBuf::from(format!("/run/user/{uid}/qream/engine.sock")))?;
    for (words, path) in [
        ("home", &paths.home),
        ("socket", &paths.socket),
        ("store", &paths.store),
    ] {
        let resolved = resolve(path)?;
        if roots.iter().any(|root| resolved.starts_with(root))
            || resolved == store
            || resolved == socket
            || resolved == workstation_socket
        {
            return Err(Error::Denied(format!(
                "Test {words} refuses the live path {} (resolved to {})",
                path.display(),
                resolved.display()
            )));
        }
    }
    let unit = paths
        .unit
        .strip_suffix(".service")
        .or_else(|| paths.unit.strip_suffix(".scope"))
        .unwrap_or(&paths.unit);
    if unit == "qream-daemon" {
        return Err(Error::Denied(format!(
            "Test unit refuses the live unit {}",
            paths.unit
        )));
    }
    for live_home in live_homes {
        for campaign in crate::registry::Registry::new(live_home.clone()).list()? {
            if unit == format!("qream-{}", campaign.name)
                || unit == campaign.session
                || unit.starts_with(&format!("qream-job-{}-", campaign.name))
            {
                return Err(Error::Denied(format!(
                    "Test unit refuses a live campaign unit {}",
                    paths.unit
                )));
            }
        }
        for owned in crate::owned::Registry::open(&live_home.join("state/owned.json")).list()? {
            if let crate::owned::Handle::Unit { name, .. } = owned.handle {
                let reserved = name
                    .strip_suffix(".service")
                    .or_else(|| name.strip_suffix(".scope"))
                    .unwrap_or(&name);
                if unit == reserved {
                    return Err(Error::Denied(format!(
                        "Test unit refuses a live campaign unit {}",
                        paths.unit
                    )));
                }
            }
        }
    }
    Ok(())
}
