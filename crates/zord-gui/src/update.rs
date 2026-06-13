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

/// Release-asset name the updater fetches the ONNX Runtime DLL under
/// (Phase 51c). ASSET-NAME CONTRACT: the Windows release workflow uploads
/// `onnxruntime.dll` under exactly this name (see release.yml Collect step).
/// Changing one side without the other silently stops the DLL from refreshing.
#[cfg(target_os = "windows")]
const ORT_DLL_ASSET: &str = "onnxruntime-windows-x64.dll";

#[derive(Clone, PartialEq)]
pub struct UpdateInfo {
    /// Newer version available upstream (no `v` prefix).
    pub version: String,
    /// Directly installable asset (the portable GUI exe) — Windows only.
    pub asset_url: Option<String>,
    /// Matching ONNX Runtime DLL for the new version — Windows only (Phase 51c).
    /// Fetched after the exe swap so a bumped ORT version isn't left stale.
    pub ort_dll_url: Option<String>,
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
    // Matching ONNX Runtime DLL for the new build (Phase 51c) — Windows only.
    #[cfg(target_os = "windows")]
    let ort_dll_url = assets
        .iter()
        .find(|(name, _)| *name == ORT_DLL_ASSET)
        .map(|(_, url)| url.clone());
    #[cfg(not(target_os = "windows"))]
    let ort_dll_url = None;
    Ok(Some(UpdateInfo {
        version,
        asset_url,
        ort_dll_url,
        page_url: format!("https://github.com/{REPO}/releases/latest"),
    }))
}

/// Download `url` and replace the running executable with it (Windows
/// portable-EXE rename-swap). Takes effect on the next launch. When
/// `ort_dll_url` is present (Phase 51c), also refresh `onnxruntime.dll` beside
/// the swapped exe so a bumped ORT version isn't left stale — a DLL failure is
/// logged and ignored (the old DLL stays, which is fine for the same ORT ver).
pub fn download_and_install(url: &str, ort_dll_url: Option<&str>) -> Result<()> {
    if !cfg!(target_os = "windows") {
        bail!("in-place update is only supported on Windows — use the download page instead");
    }
    let tmp = std::env::temp_dir().join("zord-update.exe");
    zord_net::download_to_file(url, &tmp, &mut |_, _| {}).context("downloading the update")?;
    // `self_replace` writes the new exe at the original path, so the new exe's
    // directory is the current exe's directory — where `setup_ort_dylib()`
    // looks for `onnxruntime.dll`.
    self_replace::self_replace(&tmp).context("swapping the executable")?;
    let _ = std::fs::remove_file(&tmp);

    // Phase 51c: refresh the ONNX Runtime DLL beside the new exe. Windows-only,
    // best-effort — never fail the (successful) exe update over the DLL.
    #[cfg(target_os = "windows")]
    if let Some(dll_url) = ort_dll_url {
        if let Err(e) = refresh_ort_dll(dll_url) {
            tracing::warn!("ort dll refresh failed (keeping existing): {e}");
        }
    }
    // Silence the unused-arg warning on non-Windows builds.
    let _ = ort_dll_url;
    Ok(())
}

/// Download the new ONNX Runtime DLL and place it beside the running exe
/// (Phase 51c). Atomic-ish: write to a temp file in the same dir, then rename
/// over the old DLL (a rename within a dir is atomic on NTFS, and the old DLL
/// isn't memory-mapped until an ONNX feature runs).
#[cfg(target_os = "windows")]
fn refresh_ort_dll(url: &str) -> Result<()> {
    let exe = std::env::current_exe().context("locating the current exe")?;
    let dir = exe
        .parent()
        .context("the exe has no parent directory")?
        .to_path_buf();
    let dest = dir.join("onnxruntime.dll");
    let tmp = dir.join("onnxruntime.dll.new");
    zord_net::download_to_file(url, &tmp, &mut |_, _| {}).context("downloading onnxruntime.dll")?;
    std::fs::rename(&tmp, &dest).context("swapping onnxruntime.dll")?;
    Ok(())
}
