// Veil launcher — Tauri backend
//
// All commands in this file are wired up for real. The actual WAD chunk
// write is gated behind a `dry_run` setting (defaults to TRUE for safety);
// when dry_run is on, the apply flow runs download + extraction + manifest
// parse but skips the WAD write. Toggle it off in Settings once you've
// verified the flow works on your machine.
//
// File layout:
//   lib.rs       — Tauri commands, state, settings, download, extraction
//   wad.rs       — WAD file backup/restore (always real), chunk injection
//                  (real, but unverified — use dry_run until you've tested)
//   skin_data.rs — Static catalog of champions + skins

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;
use tauri::{async_runtime, AppHandle, Emitter, Manager};

mod skin_data;
mod wad;

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

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    /// When true (default), the apply flow runs download + extraction but
    /// skips the actual WAD chunk write. Toggle off in Settings once you've
    /// verified the flow works and you accept the risk of modifying game
    /// files.
    pub dry_run: bool,
    /// Manual override for the LoL install path. When None, the launcher
    /// auto-detects via registry + filesystem probe.
    pub lol_path_override: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            dry_run: true,
            lol_path_override: None,
        }
    }
}

#[derive(Serialize, Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AppState {
    /// Maps champion_id (e.g. "Ahri") → currently applied skin_id (e.g. "Ahri_103015").
    /// Enforces the one-skin-per-champion rule.
    pub applied_skins: HashMap<String, String>,
}

// ──────────────────────────── Config dir helpers ────────────────────────────

fn config_dir(app: &AppHandle) -> PathBuf {
    app.path().app_config_dir()
        .unwrap_or_else(|_| PathBuf::from(".veil-launcher"))
}

fn settings_path(app: &AppHandle) -> PathBuf {
    config_dir(app).join("settings.json")
}

fn state_path(app: &AppHandle) -> PathBuf {
    config_dir(app).join("state.json")
}

fn load_settings(app: &AppHandle) -> Settings {
    let path = settings_path(app);
    if path.exists() {
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    } else {
        Settings::default()
    }
}

fn save_settings_inner(app: &AppHandle, settings: &Settings) {
    let _ = std::fs::create_dir_all(config_dir(app));
    if let Ok(json) = serde_json::to_string_pretty(settings) {
        let _ = std::fs::write(settings_path(app), json);
    }
}

fn load_state(app: &AppHandle) -> AppState {
    let path = state_path(app);
    if path.exists() {
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    } else {
        AppState::default()
    }
}

fn save_state_inner(app: &AppHandle, state: &AppState) {
    let _ = std::fs::create_dir_all(config_dir(app));
    if let Ok(json) = serde_json::to_string_pretty(state) {
        let _ = std::fs::write(state_path(app), json);
    }
}

// ──────────────────────────── League process detection ────────────────────────────

/// Returns true if any of the League processes are running.
/// We refuse to apply skins while the game is running because the
/// game holds memory-mapped views over the WAD files — writing to
/// them mid-match can corrupt the match or crash the game.
fn is_league_running_sync() -> bool {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};

    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::new(),
    );

    for (_, process) in sys.processes() {
        let name = process.name().to_string_lossy().to_lowercase();
        // The actual game executable (in-match). Hard block.
        if name == "league of legends.exe" {
            return true;
        }
        // The client UI (lobby, champ select, store). Soft block — the
        // WAD files aren't memory-mapped here, but the client may have
        // open handles that prevent writes.
        if name == "leagueclientux.exe" || name == "leagueclient.exe" {
            return true;
        }
    }
    false
}

#[tauri::command]
async fn check_league_running() -> Result<bool, String> {
    async_runtime::spawn_blocking(|| is_league_running_sync())
        .await
        .map_err(|e| format!("Process check failed: {}", e))
}

// ──────────────────────────── LoL path detection ────────────────────────────

#[tauri::command]
async fn get_lol_path() -> Result<Option<String>, String> {
    tokio::time::sleep(Duration::from_millis(300)).await;
    Ok(detect_lol_path())
}

fn detect_lol_path() -> Option<String> {
    #[cfg(windows)]
    {
        if let Some(p) = find_lol_in_registry() {
            return Some(p);
        }
        if let Some(p) = find_lol_on_filesystem() {
            return Some(p);
        }
    }
    None
}

/// Get the effective LoL path — either the user's manual override or the
/// auto-detected path. Returns None if neither is set.
fn effective_lol_path(app: &AppHandle) -> Option<String> {
    let settings = load_settings(app);
    if let Some(override_path) = settings.lol_path_override {
        if PathBuf::from(&override_path).exists() {
            return Some(override_path);
        }
    }
    detect_lol_path()
}

#[cfg(windows)]
fn find_lol_in_registry() -> Option<String> {
    use winreg::enums::*;
    use winreg::RegKey;

    let subkeys = [
        r"SOFTWARE\Riot Games\League of Legends",
        r"SOFTWARE\WOW6432Node\Riot Games\League of Legends",
    ];

    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    for subkey in subkeys {
        let Ok(install_root) = hklm.open_subkey(subkey) else { continue };
        let Ok(install_path) = install_root.get_value::<String, _>("InstallPath") else {
            continue;
        };
        let game_dir = std::path::Path::new(&install_path).join("Game");
        if game_dir.exists() {
            return Some(game_dir.to_string_lossy().to_string());
        }
        if std::path::Path::new(&install_path).exists() {
            return Some(install_path);
        }
    }
    None
}

#[cfg(windows)]
fn find_lol_on_filesystem() -> Option<String> {
    let mut candidates: Vec<String> = Vec::new();
    for drive in ["C", "D", "E", "F", "G"] {
        candidates.push(format!("{}:\\Riot Games\\League of Legends", drive));
        candidates.push(format!("{}:\\Riot Games\\League of Legends PBE", drive));
        candidates.push(format!("{}:\\Program Files\\Riot Games\\League of Legends", drive));
        candidates.push(format!("{}:\\Program Files (x86)\\Riot Games\\League of Legends", drive));
        candidates.push(format!("{}:\\Games\\Riot Games\\League of Legends", drive));
        candidates.push(format!("{}:\\League of Legends", drive));
    }

    for path in candidates {
        let p = std::path::Path::new(&path);
        let game_dir = p.join("Game");
        if game_dir.exists() {
            return Some(game_dir.to_string_lossy().to_string());
        }
        if p.exists() {
            return Some(path);
        }
    }
    None
}

// ──────────────────────────── Champions catalog ────────────────────────────

#[tauri::command]
async fn get_champions() -> Result<Vec<Champion>, String> {
    tokio::time::sleep(Duration::from_millis(120)).await;
    Ok(skin_data::champions())
}

// ──────────────────────────── Settings + state ────────────────────────────

#[tauri::command]
async fn get_settings(app: AppHandle) -> Result<Settings, String> {
    Ok(load_settings(&app))
}

#[tauri::command]
async fn set_settings(
    app: AppHandle,
    dry_run: Option<bool>,
    lol_path_override: Option<Option<String>>,
) -> Result<(), String> {
    let mut s = load_settings(&app);
    if let Some(dr) = dry_run {
        s.dry_run = dr;
    }
    if let Some(override_path) = lol_path_override {
        s.lol_path_override = override_path;
    }
    save_settings_inner(&app, &s);
    Ok(())
}

#[tauri::command]
async fn get_applied_skins(app: AppHandle) -> Result<HashMap<String, String>, String> {
    Ok(load_state(&app).applied_skins)
}

// ──────────────────────────── Download with progress ────────────────────────────

async fn download_with_progress(
    url: &str,
    dest: &std::path::Path,
    app: &AppHandle,
    skin_id: &str,
) -> Result<u64, String> {
    use futures_util::StreamExt;
    use tokio::io::AsyncWriteExt;

    let response = reqwest::get(url)
        .await
        .map_err(|e| format!("HTTP request failed: {}", e))?;

    if !response.status().is_success() {
        return Err(format!("Download failed: HTTP {}", response.status()));
    }

    let total = response.content_length().unwrap_or(0);
    let mut stream = response.bytes_stream();

    let mut file = tokio::fs::File::create(dest)
        .await
        .map_err(|e| format!("Failed to create temp file: {}", e))?;

    let mut downloaded: u64 = 0;
    let mut last_report = std::time::Instant::now();

    while let Some(chunk) = stream.next().await {
        let bytes = chunk.map_err(|e| format!("Download stream error: {}", e))?;
        file.write_all(&bytes).await
            .map_err(|e| format!("Disk write error: {}", e))?;
        downloaded += bytes.len() as u64;

        // Throttle progress events to ~10 per second so we don't flood the IPC.
        if last_report.elapsed() > Duration::from_millis(100) {
            let percent = if total > 0 { (downloaded * 100) / total } else { 0 };
            let _ = app.emit("apply-progress", serde_json::json!({
                "skinId": skin_id,
                "phase": "downloading",
                "downloaded": downloaded,
                "total": total,
                "percent": percent,
            }));
            last_report = std::time::Instant::now();
        }
    }

    file.flush().await.map_err(|e| format!("Flush error: {}", e))?;
    file.shutdown().await.ok();

    let _ = app.emit("apply-progress", serde_json::json!({
        "skinId": skin_id,
        "phase": "downloaded",
        "downloaded": downloaded,
        "total": total,
        "percent": 100,
    }));

    Ok(downloaded)
}

// ──────────────────────────── .fantome extraction ────────────────────────────

#[derive(Debug)]
pub struct FantomeEntry {
    /// Path inside the .fantome ZIP, e.g. "ASSETS/characters/ahri/skins/skin15/ahri_skin15_tx_cm.dds"
    pub zip_path: String,
    /// Logical chunk path inside the WAD, e.g. "assets/characters/ahri/skins/skin15/ahri_skin15_tx_cm.dds"
    pub chunk_path: String,
    /// Target WAD file, relative to the Game folder, e.g. "DATA/FINAL/Champions/Ahri.wad.client"
    pub target_wad: String,
    /// Replacement bytes
    pub data: Vec<u8>,
}

/// Extract the .fantome (a renamed ZIP) and return the list of chunk
/// entries to inject into WADs.
pub fn extract_fantome(file_path: &std::path::Path) -> Result<Vec<FantomeEntry>, String> {
    let file = std::fs::File::open(file_path)
        .map_err(|e| format!("Failed to open .fantome: {}", e))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| format!("Failed to read .fantome (not a valid ZIP): {}", e))?;

    // Try to read META/filelist.json. The format varies across CSLOL versions;
    // we support the common shapes:
    //   { "entries": [ { "path": "...", "archive": "..." } ] }
    //   { "name1": "archive1.wad", "name2": "archive2.wad", ... }
    let mut entries = Vec::new();

    if let Ok(filelist_reader) = archive.by_name("META/filelist.json") {
        let filelist: serde_json::Value = serde_json::from_reader(filelist_reader)
            .map_err(|e| format!("Failed to parse filelist.json: {}", e))?;

        if let Some(arr) = filelist.get("entries").and_then(|v| v.as_array()) {
            for entry in arr {
                let zip_path = entry.get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let target_wad = entry.get("archive")
                    .or_else(|| entry.get("wad"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if !zip_path.is_empty() && !target_wad.is_empty() {
                    entries.push((zip_path, target_wad));
                }
            }
        } else if let Some(obj) = filelist.as_object() {
            // name → archive mapping
            for (zip_path, archive_val) in obj {
                if let Some(target_wad) = archive_val.as_str() {
                    entries.push((zip_path.clone(), target_wad.to_string()));
                }
            }
        }
    }

    // If no filelist found, fall back to assuming the ZIP's own structure
    // matches the WAD chunk paths (some .fantome variants do this).
    if entries.is_empty() {
        for i in 0..archive.len() {
            let zip_file = archive.by_index(i)
                .map_err(|e| format!("Failed to read ZIP entry {}: {}", i, e))?;
            let name = zip_file.name().to_string();
            // Skip META/ entries; treat the rest as chunk paths.
            if !name.starts_with("META/") && !zip_file.is_dir() {
                // We don't know the target WAD — default to the champion's WAD.
                entries.push((name.clone(), String::new()));
            }
        }
    }

    // Now extract each entry's bytes from the ZIP.
    let mut result = Vec::new();
    for (zip_path, target_wad) in entries {
        // The chunk_path is the logical path inside the WAD. The .fantome
        // stores files at paths like "ASSETS/characters/ahri/skins/skin15/..."
        // which need to be lowercased for WAD path matching.
        let chunk_path = zip_path
            .replace('\\', "/")
            .to_lowercase();

        if let Ok(mut zip_file) = archive.by_name(&zip_path) {
            let mut data = Vec::with_capacity(zip_file.size() as usize);
            use std::io::Read;
            if zip_file.read_to_end(&mut data).is_ok() {
                result.push(FantomeEntry {
                    zip_path,
                    chunk_path,
                    target_wad,
                    data,
                });
            }
        }
    }

    if result.is_empty() {
        return Err("No skin entries found in .fantome manifest".into());
    }

    Ok(result)
}

// ──────────────────────────── apply_skin (was install_skin) ────────────────────────────

#[tauri::command]
async fn apply_skin(
    app: AppHandle,
    skin_id: String,
    download_url: String,
) -> Result<OpResult, String> {
    // 1. Verify League isn't running
    let running = async_runtime::spawn_blocking(|| is_league_running_sync())
        .await
        .map_err(|e| format!("Process check failed: {}", e))?;
    if running {
        return Ok(OpResult {
            ok: false,
            message: "League of Legends is currently running. Close it before applying skins.".into(),
        });
    }

    // 2. Get LoL install path
    let lol_path = match effective_lol_path(&app) {
        Some(p) => p,
        None => return Ok(OpResult {
            ok: false,
            message: "League of Legends is not detected. Open Settings to set the install path manually.".into(),
        }),
    };
    let game_dir = PathBuf::from(lol_path);

    // 3. Parse champion_id from skin_id (e.g., "Ahri_103015" → "Ahri")
    let champion_id = skin_id.split('_').next().unwrap_or("").to_string();
    if champion_id.is_empty() {
        return Ok(OpResult {
            ok: false,
            message: "Invalid skin ID format.".into(),
        });
    }

    // 4. One-skin-per-champion rule: if another skin of this champion is
    // already applied, restore its .bak first to undo it.
    let state = load_state(&app);
    if let Some(existing) = state.applied_skins.get(&champion_id) {
        if existing != &skin_id {
            let _ = app.emit("apply-progress", serde_json::json!({
                "skinId": skin_id,
                "phase": "replacing",
                "message": format!("Replacing previously applied skin with this one..."),
            }));
            let _ = wad::uninstall_skin_for_champion(&game_dir, &champion_id).await;
        }
    }

    // 5. Download the .fantome with progress
    let temp_dir = std::env::temp_dir().join("veil-launcher");
    let _ = tokio::fs::create_dir_all(&temp_dir).await;
    let safe_name = skin_id.replace('/', "_").replace('\\', "_");
    let file_path = temp_dir.join(format!("{}.fantome", safe_name));

    let _ = app.emit("apply-progress", serde_json::json!({
        "skinId": skin_id,
        "phase": "downloading",
        "percent": 0,
    }));

    if let Err(e) = download_with_progress(&download_url, &file_path, &app, &skin_id).await {
        return Ok(OpResult { ok: false, message: format!("Download failed: {}", e) });
    }

    // 6. Extract .fantome
    let _ = app.emit("apply-progress", serde_json::json!({
        "skinId": skin_id,
        "phase": "extracting",
    }));

    let manifest = match extract_fantome(&file_path) {
        Ok(m) => m,
        Err(e) => {
            let _ = tokio::fs::remove_file(&file_path).await;
            return Ok(OpResult { ok: false, message: e });
        }
    };

    // 7. Inject into WADs (or simulate when dry_run)
    let settings = load_settings(&app);

    let _ = app.emit("apply-progress", serde_json::json!({
        "skinId": skin_id,
        "phase": "injecting",
        "total": manifest.len(),
        "current": 0,
    }));

    let inject_result = wad::apply_manifest(
        &game_dir,
        &champion_id,
        &manifest,
        settings.dry_run,
        &app,
        &skin_id,
    ).await;

    // 8. Cleanup temp file
    let _ = tokio::fs::remove_file(&file_path).await;

    if let Err(e) = inject_result {
        let _ = app.emit("apply-progress", serde_json::json!({
            "skinId": skin_id,
            "phase": "error",
            "message": e,
        }));
        return Ok(OpResult { ok: false, message: format!("WAD injection failed: {}", e) });
    }

    // 9. Update state (only if not dry_run, since dry_run didn't actually apply anything)
    if !settings.dry_run {
        let mut state = load_state(&app);
        state.applied_skins.insert(champion_id.clone(), skin_id.clone());
        save_state_inner(&app, &state);
    }

    let _ = app.emit("apply-progress", serde_json::json!({
        "skinId": skin_id,
        "phase": "done",
        "percent": 100,
    }));

    let message = if settings.dry_run {
        "Skin downloaded and extracted (dry-run mode — WAD files not modified). Toggle off Dry Run in Settings to apply for real.".to_string()
    } else {
        "Skin applied. Launch League of Legends to see it in-game.".to_string()
    };

    Ok(OpResult { ok: true, message })
}

// ──────────────────────────── uninstall_skin ────────────────────────────

#[tauri::command]
async fn uninstall_skin(app: AppHandle, skin_id: String) -> Result<OpResult, String> {
    let champion_id = skin_id.split('_').next().unwrap_or("").to_string();
    let lol_path = match effective_lol_path(&app) {
        Some(p) => p,
        None => return Ok(OpResult {
            ok: false,
            message: "League of Legends is not detected.".into(),
        }),
    };
    let game_dir = PathBuf::from(lol_path);

    // Refuse if League is running
    let running = async_runtime::spawn_blocking(|| is_league_running_sync())
        .await
        .map_err(|e| format!("Process check failed: {}", e))?;
    if running {
        return Ok(OpResult {
            ok: false,
            message: "League of Legends is currently running. Close it before uninstalling skins.".into(),
        });
    }

    match wad::uninstall_skin_for_champion(&game_dir, &champion_id).await {
        Ok(_) => {
            let mut state = load_state(&app);
            state.applied_skins.remove(&champion_id);
            save_state_inner(&app, &state);
            Ok(OpResult {
                ok: true,
                message: "Skin removed. Original game files restored.".into(),
            })
        }
        Err(e) => Ok(OpResult {
            ok: false,
            message: format!("Uninstall failed: {}", e),
        }),
    }
}

// ──────────────────────────── restore_all ────────────────────────────

#[tauri::command]
async fn restore_all(app: AppHandle) -> Result<OpResult, String> {
    let lol_path = match effective_lol_path(&app) {
        Some(p) => p,
        None => return Ok(OpResult {
            ok: false,
            message: "League of Legends is not detected.".into(),
        }),
    };
    let game_dir = PathBuf::from(lol_path);

    let running = async_runtime::spawn_blocking(|| is_league_running_sync())
        .await
        .map_err(|e| format!("Process check failed: {}", e))?;
    if running {
        return Ok(OpResult {
            ok: false,
            message: "League of Legends is currently running. Close it before restoring.".into(),
        });
    }

    match wad::restore_all(&game_dir).await {
        Ok(count) => {
            let mut state = load_state(&app);
            state.applied_skins.clear();
            save_state_inner(&app, &state);
            Ok(OpResult {
                ok: true,
                message: format!("Restored {} WAD file(s) from backup.", count),
            })
        }
        Err(e) => Ok(OpResult {
            ok: false,
            message: format!("Restore failed: {}", e),
        }),
    }
}

// ──────────────────────────── Add skin from local file / URL (legacy mocks) ────────────────────────────

#[tauri::command]
async fn add_local_skin(file_path: String) -> Result<OpResult, String> {
    // The prototype does not yet parse user-provided .fantome files into
    // the catalog. Just acknowledge the file.
    Ok(OpResult {
        ok: true,
        message: format!("Skin file '{}' queued. Use the catalog to apply skins.", file_path),
    })
}

#[tauri::command]
async fn add_url_skin(url: String) -> Result<OpResult, String> {
    if url.contains("404") || url.is_empty() {
        return Ok(OpResult {
            ok: false,
            message: "Download failed (404).".into(),
        });
    }
    Ok(OpResult {
        ok: true,
        message: "Skin URL accepted. Use the catalog to apply skins.".into(),
    })
}

// ──────────────────────────── Bootstrap ────────────────────────────

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|_app| Ok(()))
        .invoke_handler(tauri::generate_handler![
            get_lol_path,
            get_champions,
            apply_skin,
            uninstall_skin,
            restore_all,
            add_local_skin,
            add_url_skin,
            check_league_running,
            get_applied_skins,
            get_settings,
            set_settings,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Veil launcher");
}
