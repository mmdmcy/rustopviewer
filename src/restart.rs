use anyhow::{Context, Result, anyhow};
use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};

const MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");

pub fn queue_source_restart() -> Result<()> {
    let repo_root = discover_repo_root()?;
    let current_pid = std::process::id();
    let cargo_args = cargo_run_args();
    let quoted_repo_root = quote_for_powershell(&repo_root);
    let command = format!(
        "$ErrorActionPreference = 'Stop'; \
         while (Get-Process -Id {current_pid} -ErrorAction SilentlyContinue) {{ Start-Sleep -Milliseconds 250; }}; \
         Set-Location -LiteralPath {quoted_repo_root}; \
         cargo {cargo_args}"
    );

    Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &command,
        ])
        .spawn()
        .context("failed to launch the restart helper")?;

    Ok(())
}

fn discover_repo_root() -> Result<PathBuf> {
    for candidate in candidate_roots() {
        if candidate.join("Cargo.toml").is_file() {
            return Ok(candidate);
        }
    }

    Err(anyhow!(
        "could not find the repository root that contains Cargo.toml"
    ))
}

fn candidate_roots() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    candidates.push(PathBuf::from(MANIFEST_DIR));

    if let Ok(current_dir) = env::current_dir() {
        candidates.push(current_dir);
    }

    if let Ok(current_exe) = env::current_exe() {
        for ancestor in current_exe.ancestors().skip(1) {
            candidates.push(ancestor.to_path_buf());
        }
    }

    candidates
}

fn cargo_run_args() -> &'static str {
    if is_release_build() {
        "run --release"
    } else {
        "run"
    }
}

fn is_release_build() -> bool {
    env::current_exe()
        .ok()
        .and_then(|path| parent_dir_name(&path))
        .is_some_and(|name| name.eq_ignore_ascii_case("release"))
}

fn parent_dir_name(path: &Path) -> Option<String> {
    path.parent()
        .and_then(|parent| parent.file_name())
        .map(|name| name.to_string_lossy().into_owned())
}

fn quote_for_powershell(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "''"))
}
