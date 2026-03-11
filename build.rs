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
    // Rebuild if HEAD or the branch ref changes
    println!("cargo:rerun-if-changed=.git/HEAD");
    // When HEAD is a symbolic ref (e.g. refs/heads/main), also watch the ref file
    if let Ok(head) = std::fs::read_to_string(".git/HEAD") {
        let head = head.trim();
        if let Some(refpath) = head.strip_prefix("ref: ") {
            println!("cargo:rerun-if-changed=.git/{}", refpath);
        }
    }
}
