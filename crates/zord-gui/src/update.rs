//! In-app update check + Windows self-install (Phase 34a, `self-update`).
//!
//! Channel-aware by design: only the GitHub / own-store distribution
//! self-updates — store builds (Steam, Microsoft Store, …) omit this feature
//! entirely because stores manage updates themselves and reject self-updating
//! binaries. `dev` builds keep the path testable.
//!
//! The Windows in-place install relies on a portable-EXE property: a running
//! executable's file can't be overwritten, but it *can* be renamed. The
//! `self-replace` crate renames the running EXE aside, writes the new one at
//! the original path, and cleans up on the next run — so the update takes
//! effect on relaunch. macOS gets notify + open-the-download-page only
//! (Gatekeeper re-quarantines unsigned downloads, so a silent swap of the
//! .app would do more harm than good until releases are signed).

use anyhow::{bail, Context, Result};

const REPO: &str = "JacobHaig/zord";
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, PartialEq)]
pub struct UpdateInfo {
    /// Newer version available upstream (no `v` prefix).
    pub version: String,
    /// Directly installable asset (the portable GUI exe) — Windows only.
    pub asset_url: Option<String>,
    /// The release page, for manual download.
    pub page_url: String,
}

/// Should this build check for updates at all? Store channels never do.
pub fn channel_self_updates() -> bool {
    matches!(zord_core::DIST_CHANNEL, "github" | "dev")
}

/// Ask GitHub for the newest release. `Ok(None)` = already up to date.
pub fn check() -> Result<Option<UpdateInfo>> {
    let (tag, assets) = zord_net::latest_github_release(REPO, std::time::Duration::from_secs(10))
        .map_err(|e| anyhow::anyhow!("update check failed: {e}"))?;
    if !zord_core::is_newer_version(CURRENT_VERSION, &tag) {
        return Ok(None);
    }
    let version = tag.trim_start_matches(['v', 'V']).to_string();
    // The portable Windows GUI exe is the only asset we can swap in place.
    let wanted = format!("Zord-{version}-windows-x64-gui.exe");
    let asset_url = if cfg!(target_os = "windows") {
        assets
            .iter()
            .find(|(name, _)| *name == wanted)
            .map(|(_, url)| url.clone())
    } else {
        None
    };
    Ok(Some(UpdateInfo {
        version,
        asset_url,
        page_url: format!("https://github.com/{REPO}/releases/latest"),
    }))
}

/// Download `url` and replace the running executable with it (Windows
/// portable-EXE rename-swap). Takes effect on the next launch.
pub fn download_and_install(url: &str) -> Result<()> {
    if !cfg!(target_os = "windows") {
        bail!("in-place update is only supported on Windows — use the download page instead");
    }
    let tmp = std::env::temp_dir().join("zord-update.exe");
    zord_net::download_to_file(url, &tmp, &mut |_, _| {}).context("downloading the update")?;
    self_replace::self_replace(&tmp).context("swapping the executable")?;
    let _ = std::fs::remove_file(&tmp);
    Ok(())
}
