fn main() {
    // Embed git commit hash at compile time
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output();
    let hash = match output {
        Ok(o) => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        Err(_) => "unknown".to_string(),
    };
    println!("cargo:rustc-env=GIT_HASH={}", hash);
    // Rebuild if HEAD changes
    println!("cargo:rerun-if-changed=.git/HEAD");
}
