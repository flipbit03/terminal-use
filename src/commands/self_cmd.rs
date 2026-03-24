use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use crate::version_check;

#[derive(Debug, Subcommand)]
pub enum SelfAction {
    /// Update tu to the latest release.
    Update(UpdateArgs),
}

#[derive(Debug, Parser)]
pub struct UpdateArgs {
    /// Just check if an update is available, don't install it.
    #[arg(long)]
    check: bool,
}

pub async fn run(action: SelfAction) -> Result<()> {
    match action {
        SelfAction::Update(args) => run_update(args).await,
    }
}

async fn run_update(args: UpdateArgs) -> Result<()> {
    if version_check::is_dev_build() {
        eprintln!("Running a dev build (0.0.0) — self-update is not supported.");
        return Ok(());
    }

    let current = version_check::current_version();
    let latest = version_check::get_latest_version()
        .await
        .context("could not determine the latest version")?;

    if args.check {
        if !version_check::is_newer(current, &latest) {
            println!("tu {current} is already the latest version.");
        } else {
            println!("Update available: {current} -> {latest}");
            println!("Run `tu self update` to upgrade.");
        }
        return Ok(());
    }

    if !version_check::is_newer(current, &latest) {
        println!("tu {current} is already the latest version.");
        return Ok(());
    }

    println!("Updating tu {current} -> {latest}");

    if cfg!(feature = "binary-release") {
        update_binary(&latest).await
    } else {
        update_cargo().await
    }
}

/// Binary mode: download the release asset and replace the current executable.
async fn update_binary(version: &str) -> Result<()> {
    let url = version_check::release_asset_url(version)?;
    let current_exe =
        std::env::current_exe().context("could not determine current executable path")?;

    println!("Downloading from GitHub Releases...");

    let client = reqwest::Client::new();
    let bytes = client
        .get(&url)
        .header("User-Agent", "tu-self-update")
        .send()
        .await
        .context("failed to download release binary")?
        .error_for_status()
        .context("download failed")?
        .bytes()
        .await
        .context("failed to read response body")?;

    // Write to a temp file next to the current binary, then rename atomically.
    let tmp_path = current_exe.with_extension("tmp-update");
    tokio::fs::write(&tmp_path, &bytes)
        .await
        .context("failed to write temporary file")?;

    // Set executable bit (unix only).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        tokio::fs::set_permissions(&tmp_path, perms)
            .await
            .context("failed to set executable permissions")?;
    }

    tokio::fs::rename(&tmp_path, &current_exe)
        .await
        .context("failed to replace binary — you may need to run with appropriate permissions")?;

    println!("tu {version} installed to {}", current_exe.display());
    Ok(())
}

/// Cargo mode: shell out to `cargo install terminal-use`.
async fn update_cargo() -> Result<()> {
    // Verify cargo is available.
    let cargo_check = std::process::Command::new("cargo")
        .arg("--version")
        .output();
    if cargo_check.is_err() || !cargo_check.unwrap().status.success() {
        anyhow::bail!(
            "tu was installed via cargo, but `cargo` is not in your PATH.\n\
             Install Rust from https://rustup.rs or add cargo to your PATH."
        );
    }

    println!("Running `cargo install terminal-use`...");

    let status = std::process::Command::new("cargo")
        .args(["install", "terminal-use"])
        .status()
        .context("failed to run `cargo install terminal-use`")?;

    if !status.success() {
        anyhow::bail!("`cargo install terminal-use` exited with status {status}");
    }

    println!("Update complete.");
    Ok(())
}
