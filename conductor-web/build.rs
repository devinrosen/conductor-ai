fn main() {
    // Ensure frontend/dist/ exists so rust-embed compiles in fresh worktrees.
    // CI and release builds populate it via `bun run build` before cargo build.
    let dist = std::path::Path::new("frontend/dist");
    if !dist.exists() {
        std::fs::create_dir_all(dist).expect("failed to create frontend/dist/");
    }
    println!("cargo:rerun-if-changed=frontend/dist");
}
