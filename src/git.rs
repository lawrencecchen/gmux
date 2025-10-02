use std::{path::Path, process::Command};

use anyhow::{Context, Result, anyhow};

pub fn current_branch(path: &Path) -> Result<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(path)
        .output()
        .with_context(|| format!("failed to invoke git in {}", path.display()))?;

    if !output.status.success() {
        return Err(anyhow!("git rev-parse failed for {}", path.display()));
    }

    let mut branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if branch == "HEAD" {
        let fallback = Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .current_dir(path)
            .output()
            .with_context(|| format!("failed to resolve HEAD for {}", path.display()))?;
        if fallback.status.success() {
            branch = format!(
                "detached@{}",
                String::from_utf8_lossy(&fallback.stdout).trim()
            );
        }
    }

    Ok(branch)
}

pub fn is_git_repo(path: &Path) -> bool {
    Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(path)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}
