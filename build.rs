fn main() {
    println!("cargo:rustc-env=DISPLAY_NAME=Agent-First PSQL");
    println!("cargo:rustc-env=GIT_SHA={}", git_sha());
}

/// Short commit SHA of the tree this was built from, or `"unknown"` when no
/// `.git` is reachable (a crates.io source tarball, for example).
fn git_sha() -> String {
    std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|sha| sha.trim().to_string())
        .filter(|sha| !sha.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}
