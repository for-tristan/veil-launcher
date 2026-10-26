// Veil — Tauri backend
//
// This file is the entry point for the desktop app. It registers all
// Tauri commands that the frontend can call. For this prototype, every
// command is mocked (returns realistic responses after a short delay)
// so the UI is fully functional without touching real game files.
//
// ── Where to wire real LoL skin logic ──────────────────────────────
// Look for the `// TODO:` markers below. The flow you want is roughly:
//
//   1. Detect LoL install path (registry on Windows)
//   2. Parse WAD files at: <lol>/Game/DATA/FINAL or via binhash map
//      Use the `lolpack` / `League of Legends WAD parser` crates, or
//      a vendored parser following the public WAD format spec.
//   3. For install: backup the original WAD entry to a `.bak` file,
//      then inject the modded asset into the WAD's chunk table.
//   4. For uninstall: restore from the `.bak` file.
//
// Recommended external crates (when you graduate this prototype):
//   - `wad` — pure-Rust WAD reader/writer (search crates.io)
//   - `tauri-plugin-dialog` — native OS file picker
//   - `tauri-plugin-fs` — file system access from JS side
//   - `reqwest` — HTTP client for downloading skin packs from URLs
//
// Keep the binary lean. Veil's whole pitch is "fast, simple,
// lightweight, minimalist" — don't pull in heavy deps unless they
// earn their keep.

use serde::{Deserialize, Serialize};
use std::time::Duration;

mod skin_data;

// ──────────────────────────── Types ────────────────────────────

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Skin {
    pub id: String,
    pub name: String,
    pub rarity: String,
    pub tier: Option<String>,
    pub art: String,
    pub download: String,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Champion {
    pub id: String,
    pub name: String,
    pub role: String,
    pub tile: String,
    pub skins: Vec<Skin>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpResult {
    pub ok: bool,
    pub message: String,
}

// ──────────────────────────── Commands ────────────────────────────

/// Returns the detected League of Legends install path.
/// In a real build, scan the Windows registry for the Riot client path,
/// then resolve the LoL Game directory from there.
#[tauri::command]
async fn get_lol_path() -> Result<String, String> {
    tokio::time::sleep(Duration::from_millis(600)).await;
    // TODO: real detection. Try:
    //   - HKLM\SOFTWARE\Riot Games\League of Legends (InstallPath)
    //   - Common paths: C:\Riot Games\League of Legends\
    //   - LeagueClientUx.exe process introspection
    Ok(r"C:\Riot Games\League of Legends\Game".to_string())
}

/// Returns the catalog of champions + skins shown in the launcher.
#[tauri::command]
async fn get_champions() -> Result<Vec<Champion>, String> {
    tokio::time::sleep(Duration::from_millis(120)).await;
    Ok(skin_data::champions())
}

/// Installs a skin by downloading its `.fantome` package and injecting its
/// assets into the appropriate WAD files.
///
/// For the prototype, this is mocked. In production:
///   1. Stream-download `download_url` to a temp file (use `reqwest`).
///   2. Open the .fantome (it's a zip) — extract its mod metadata.
///   3. For each (target_wad, chunk_path, asset_data) entry:
///        - Open the WAD file
///        - Look up the chunk by `xxhash64(chunk_path)`
///        - Back up the original chunk to `<wad>.bak`
///        - Write the new chunk data + update the chunk table
///   4. Flush files, return Ok if all targets succeeded.
///
/// `skin_id` follows Riot's format: `{champ_key}{3-digit_skin_index}`
/// (e.g. `103015` for K/DA Ahri).
#[tauri::command]
async fn install_skin(skin_id: String, download_url: String) -> Result<OpResult, String> {
    tokio::time::sleep(Duration::from_millis(700)).await;
    let _ = (&skin_id, &download_url);
    Ok(OpResult {
        ok: true,
        message: "Skin downloaded & injected successfully.".into(),
    })
}

/// Removes a previously installed skin, restoring the original game asset
/// from its .bak backup.
#[tauri::command]
async fn uninstall_skin(skin_id: String) -> Result<OpResult, String> {
    tokio::time::sleep(Duration::from_millis(500)).await;
    // TODO: implement real uninstall (restore from .bak)
    let _ = &skin_id;
    Ok(OpResult {
        ok: true,
        message: "Skin removed.".into(),
    })
}

/// Restores all modified WAD files to their original state.
/// Useful as a panic button before a LoL patch lands.
#[tauri::command]
async fn restore_all() -> Result<OpResult, String> {
    tokio::time::sleep(Duration::from_millis(1200)).await;
    // TODO: walk all .bak files in the LoL Game dir, restore each one
    Ok(OpResult {
        ok: true,
        message: "All skins restored to original.".into(),
    })
}

/// Adds a skin package from a local file path (.wad.client or .zip).
/// In a real build, this would:
///   - Validate the file is a proper WAD/ZIP
///   - Extract its manifest (champion, skin_id, target paths)
///   - Register it in the launcher's library
#[tauri::command]
async fn add_local_skin(file_path: String) -> Result<OpResult, String> {
    tokio::time::sleep(Duration::from_millis(900)).await;
    // TODO: parse + validate the file
    Ok(OpResult {
        ok: true,
        message: format!("Added: {}", file_path),
    })
}

/// Downloads a skin package from a URL and adds it to the library.
/// In a real build, use `reqwest` to stream the download to a temp file,
/// then run the same logic as `add_local_skin`.
#[tauri::command]
async fn add_url_skin(url: String) -> Result<OpResult, String> {
    tokio::time::sleep(Duration::from_millis(1100)).await;
    if url.contains("404") || url.is_empty() {
        return Ok(OpResult {
            ok: false,
            message: "Download failed (404).".into(),
        });
    }
    Ok(OpResult {
        ok: true,
        message: "Skin downloaded & added.".into(),
    })
}

// ──────────────────────────── Bootstrap ────────────────────────────

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|_app| {
            // You can wire app-level state here (database, config, etc.)
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_lol_path,
            get_champions,
            install_skin,
            uninstall_skin,
            restore_all,
            add_local_skin,
            add_url_skin,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Veil application");
}
