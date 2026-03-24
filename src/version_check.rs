use anyhow::{Context, Result};

const GITHUB_REPO: &str = "flipbit03/terminal-use";

/// Fetch the latest release version from GitHub.
pub async fn get_latest_version() -> Result<String> {
    let url = format!("https://api.github.com/repos/{GITHUB_REPO}/releases/latest");
    let client = reqwest::Client::new();
    let resp: serde_json::Value = client
        .get(&url)
        .header("User-Agent", "tu-self-update")
        .send()
        .await
        .context("failed to reach GitHub API")?
        .error_for_status()
        .context("GitHub API returned an error")?
        .json()
        .await
        .context("failed to parse GitHub API response")?;

    let tag = resp["tag_name"]
        .as_str()
        .context("no tag_name in GitHub release")?;
    Ok(tag.strip_prefix('v').unwrap_or(tag).to_string())
}

/// Returns the current compiled-in version of tu.
pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Returns true if this is a dev build (version 0.0.0).
pub fn is_dev_build() -> bool {
    current_version() == "0.0.0"
}

/// Returns true if `latest` is newer than `current` using semver comparison.
pub fn is_newer(current: &str, latest: &str) -> bool {
    match (
        semver::Version::parse(current),
        semver::Version::parse(latest),
    ) {
        (Ok(c), Ok(l)) => l > c,
        _ => latest != current,
    }
}

/// Returns the download URL for a GitHub release asset for the current platform.
pub fn release_asset_url(tag: &str) -> Result<String> {
    let os_tag = match std::env::consts::OS {
        "linux" => "linux",
        "macos" => "macos",
        _ => anyhow::bail!("unsupported OS: {}", std::env::consts::OS),
    };
    let arch_tag = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        _ => anyhow::bail!("unsupported architecture: {}", std::env::consts::ARCH),
    };

    if os_tag == "macos" && arch_tag == "x86_64" {
        anyhow::bail!("macOS x86_64 binaries are not provided — use `cargo install terminal-use`");
    }

    Ok(format!(
        "https://github.com/{GITHUB_REPO}/releases/download/v{tag}/tu_{os_tag}_{arch_tag}"
    ))
}
