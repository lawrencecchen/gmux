use std::{path::Path, process::Command};

use anyhow::{Context, Result, anyhow};

#[derive(Debug, Default, Clone, Copy)]
pub struct DiffStat {
    pub additions: u32,
    pub deletions: u32,
}

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

pub fn diff_stat(path: &Path) -> Result<DiffStat> {
    let commands: &[&[&str]] = &[&["diff", "--shortstat", "HEAD"], &["diff", "--shortstat"]];

    for args in commands {
        let output = Command::new("git")
            .args(*args)
            .current_dir(path)
            .output()
            .with_context(|| format!("failed to invoke git diff in {}", path.display()))?;

        if output.status.success() {
            return Ok(parse_shortstat(&output.stdout));
        }
    }

    Err(anyhow!("git diff failed for {}", path.display()))
}

fn parse_shortstat(stdout: &[u8]) -> DiffStat {
    let mut stat = DiffStat::default();
    let text = String::from_utf8_lossy(stdout);

    for part in text.split(',') {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            continue;
        }

        if trimmed.contains("insertion") {
            if let Some(value) = extract_number(trimmed) {
                stat.additions = value;
            }
        } else if trimmed.contains("deletion") {
            if let Some(value) = extract_number(trimmed) {
                stat.deletions = value;
            }
        }
    }

    stat
}

fn extract_number(text: &str) -> Option<u32> {
    text.split_whitespace()
        .find(|token| token.chars().all(|ch| ch.is_ascii_digit()))
        .and_then(|token| token.parse().ok())
}

pub fn is_git_repo(path: &Path) -> bool {
    Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(path)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}
