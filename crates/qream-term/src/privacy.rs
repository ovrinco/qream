//! The private folder is never reachable from a pane (RW-17-R03): every spawn is refused
//! when its working directory, an argument or an environment value lies under the
//! operator's private folder. The check is lexical and never opens the folder.
use crate::{Error, Result, pane::PtySpec};
use std::path::{Component, Path, PathBuf};

/// `QREAM_PRIVATE_DIR` when set (tests), else `$HOME/.local/share/ovrin-private`.
pub fn private_dir() -> Option<PathBuf> {
    let var = |key: &str| std::env::var_os(key).filter(|v| !v.is_empty());
    var("QREAM_PRIVATE_DIR")
        .map(PathBuf::from)
        .or_else(|| var("HOME").map(|h| PathBuf::from(h).join(".local/share/ovrin-private")))
}

/// The path with `.` and `..` folded, without touching the disk.
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

fn under(candidate: &str, private: &Path, cwd: &Path) -> bool {
    // An argument may carry the path after `=` (`--file=/…`) or as a word inside it.
    candidate
        .split(['=', ' ', ':', ','])
        .filter(|word| !word.is_empty())
        .any(|word| {
            let word = word.strip_prefix('~').map_or_else(
                || PathBuf::from(word),
                |rest| {
                    std::env::var_os("HOME")
                        .map(PathBuf::from)
                        .unwrap_or_default()
                        .join(rest.trim_start_matches('/'))
                },
            );
            let full = if word.is_absolute() {
                word
            } else {
                cwd.join(word)
            };
            lexical(&full).starts_with(private)
        })
}

/// Refuse a spawn that would reach the private folder.
pub fn check_spawn(spec: &PtySpec) -> Result<()> {
    match private_dir() {
        Some(private) => check_spawn_against(spec, &private),
        None => Ok(()),
    }
}

/// Refuse a spawn whose working directory, arguments or environment reach `private`.
pub fn check_spawn_against(spec: &PtySpec, private: &Path) -> Result<()> {
    let private = lexical(private);
    let refuse = |what: &str| {
        Err(Error::Invalid(format!(
            "the {what} reaches the operator's private folder; a pane never does"
        )))
    };
    let cwd = lexical(&spec.cwd);
    if cwd.starts_with(&private) {
        return refuse("working directory");
    }
    if spec.argv.iter().any(|a| under(a, &private, &cwd)) {
        return refuse("command");
    }
    if spec
        .env
        .values()
        .any(|v| v.contains(&*private.to_string_lossy()))
    {
        return refuse("environment");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_under_the_folder_are_found_lexically() {
        let p = Path::new("/h/.local/share/ovrin-private");
        let cwd = Path::new("/h/work");
        assert!(under("/h/.local/share/ovrin-private/a.md", p, cwd));
        assert!(under("--file=/h/.local/share/ovrin-private", p, cwd));
        assert!(under("../.local/share/ovrin-private/x", p, cwd));
        assert!(!under("/h/.local/share/ovrin-privateer", p, cwd));
        assert!(!under("cat notes.md", p, cwd));
    }

    #[test]
    fn a_spawn_reaching_the_folder_is_refused_by_cwd_argument_or_environment() {
        let p = Path::new("/h/.local/share/ovrin-private");
        let spec = |argv: &[&str], cwd: &str, env: &[(&str, &str)]| PtySpec {
            argv: argv.iter().map(|s| (*s).to_owned()).collect(),
            cwd: cwd.into(),
            env: env
                .iter()
                .map(|(k, v)| ((*k).into(), (*v).into()))
                .collect(),
            cols: 80,
            rows: 24,
        };
        let refused = |s: PtySpec| check_spawn_against(&s, p).is_err();
        assert!(refused(spec(
            &["sh"],
            "/h/.local/share/ovrin-private/x",
            &[]
        )));
        assert!(refused(spec(
            &["cat", "/h/.local/share/ovrin-private/a"],
            "/h",
            &[]
        )));
        assert!(refused(spec(
            &["sh"],
            "/h",
            &[("NOTES", "/h/.local/share/ovrin-private")]
        )));
        assert!(!refused(spec(&["sh"], "/h/work", &[("SHELL", "/bin/sh")])));
    }
}
