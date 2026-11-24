use std::process::Command;

fn output(program: &str, args: &[&str]) -> Option<String> {
    let result = Command::new(program).args(args).output().ok()?;
    if !result.status.success() {
        return None;
    }
    String::from_utf8(result.stdout)
        .ok()
        .map(|s| s.trim().to_owned())
}

fn main() -> std::io::Result<()> {
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/heads/rebuild-T10");
    println!("cargo:rerun-if-changed=build.rs");
    if let Some(description) = output("git", &["describe", "--always", "--dirty", "--tags"]) {
        println!("cargo:rustc-env=QREAM_GIT_DESCRIBE={description}");
    }
    let time = output("date", &["-u", "+%Y-%m-%dT%H:%M:%SZ"]).ok_or_else(|| {
        std::io::Error::other("date failed to provide the required build timestamp")
    })?;
    println!("cargo:rustc-env=QREAM_BUILD_TIME={time}");
    Ok(())
}
