//! Kong spec runner — runs Kong official spec files via busted against Kong-Rust
//! Kong spec 运行器 — 通过 busted 对 Kong-Rust 运行 Kong 官方 spec 文件

use std::env;
use std::path::PathBuf;
use std::process::Command;

/// Find workspace root via cargo metadata
/// 通过 cargo metadata 定位 workspace 根目录
fn workspace_root() -> PathBuf {
    let output = Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version=1"])
        .output()
        .expect("Failed to run cargo metadata");
    let metadata: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("Failed to parse cargo metadata");
    PathBuf::from(metadata["workspace_root"].as_str().unwrap())
}

/// Run busted on a spec file or directory — 在 spec 文件或目录上运行 busted
fn run_busted(spec_path: &str) -> bool {
    let root = workspace_root();

    // Get luarocks paths — 获取 luarocks 路径
    let luarocks_output = Command::new("luarocks")
        .args(["path", "--bin"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();

    // Parse LUA_PATH and LUA_CPATH from luarocks output — 解析 luarocks 输出的路径
    let mut lua_path = String::new();
    let mut lua_cpath = String::new();
    let mut path_env = env::var("PATH").unwrap_or_default();

    for line in luarocks_output.lines() {
        if line.starts_with("export LUA_PATH='") {
            lua_path = line
                .trim_start_matches("export LUA_PATH='")
                .trim_end_matches('\'')
                .to_string();
        } else if line.starts_with("export LUA_CPATH='") {
            lua_cpath = line
                .trim_start_matches("export LUA_CPATH='")
                .trim_end_matches('\'')
                .to_string();
        } else if line.starts_with("export PATH='") {
            let p = line
                .trim_start_matches("export PATH='")
                .trim_end_matches('\'');
            path_env = format!("{}:{}", p, path_env);
        }
    }

    // Add spec/ to Lua path — 将 spec/ 添加到 Lua 路径
    let spec_lua_path = format!(
        "{root}/spec/?.lua;{root}/spec/?/init.lua;{root}/?.lua;{lua_path}",
        root = root.display(),
    );

    let status = Command::new("busted")
        .args([
            "--helper=spec/helpers.lua",
            "-o", "TAP",
            "--no-auto-insulate",
            spec_path,
        ])
        .current_dir(&root)
        .env("LUA_PATH", &spec_lua_path)
        .env("LUA_CPATH", &lua_cpath)
        .env("PATH", &path_env)
        .env("KONG_RUST_BIN", root.join("target/debug/kong").to_str().unwrap())
        .status()
        .expect("Failed to execute busted — is it installed? Run: make setup-busted");

    status.success()
}

#[test]
#[ignore] // Run with: cargo test --test spec_runner -- --ignored
fn test_run_kong_specs() {
    assert!(
        run_busted("spec/"),
        "Kong spec tests failed — Kong spec 测试失败"
    );
}
