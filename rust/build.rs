use std::path::Path;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=../web/src");
    println!("cargo:rerun-if-changed=../web/index.html");
    println!("cargo:rerun-if-changed=../web/package.json");
    println!("cargo:rerun-if-changed=../web/vite.config.ts");

    let web_dir = Path::new("../web");
    let dist_dir = web_dir.join("dist");
    let npm = if cfg!(windows) { "npm.cmd" } else { "npm" };

    // Ensure dist directory exists so rust-embed derive doesn't fail
    std::fs::create_dir_all(&dist_dir).expect("Failed to create web/dist directory");

    // Skip frontend build if SKIP_FRONTEND_BUILD is set (e.g. in CI or dev)
    if std::env::var("SKIP_FRONTEND_BUILD").is_ok() {
        return;
    }

    if !web_dir.join("node_modules").exists() {
        let status = Command::new(npm)
            .arg("install")
            .current_dir(web_dir)
            .status()
            .expect("Failed to run npm install");
        assert!(status.success(), "npm install failed");
    }

    let status = Command::new(npm)
        .args(["run", "build"])
        .current_dir(web_dir)
        .status()
        .expect("Failed to run npm build");
    assert!(status.success(), "npm build failed");
}
