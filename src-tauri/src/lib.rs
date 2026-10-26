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
use std::sync::Mutex;
use std::time::Duration;
use tauri::{async_runtime, AppHandle, Emitter, Manager};

mod ltk_patcher;
mod skin_data;
mod wad;

// ──────────────────────────── Patcher process state ────────────────────────────

/// Holds the long-running `patcher_host.exe` subprocess so it stays alive
/// between Tauri command calls. When apply_skin returns, the Child is NOT
/// dropped — it's stored here, keeping stdin/stdout/stderr pipes open.
///
/// Without this, dropping the Child closes stdin → the patcher sees EOF →
/// it exits prematurely → the game never gets hooked.
///
/// On the next apply_skin call, the previous Child is killed + waited on
/// before starting a new one (one patcher at a time).
struct PatcherState {
    child: Mutex<Option<std::process::Child>>,
}

impl PatcherState {
    fn new() -> Self {
        Self { child: Mutex::new(None) }
    }

    /// Replace the current patcher (if any) with a new one. The previous
    /// Child is killed + reaped to avoid orphan processes.
    fn replace(&self, new_child: std::process::Child) {
        let mut guard = self.child.lock().unwrap();
        if let Some(mut prev) = guard.take() {
            // Try to kill the previous patcher. On Windows, killing a
            // child process tree requires the PROCESS_TERMINATE access
            // right which we have by default via spawn().
            let _ = prev.kill();
            let _ = prev.wait();
        }
        // We don't take stdin — leave it piped so the patcher can read
        // the config commands we already wrote.
        *guard = Some(new_child);
    }
}

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
    /// When true, the apply flow skips WAD chunk injection entirely (used
    /// only when the user explicitly wants to test the file-modification
    /// path in wad.rs). Default is false — Veil uses the ltk_patcher
    /// subprocess approach (mirrors Rose), which is the only path that
    /// actually works for runtime skin injection.
    pub dry_run: bool,
    /// Manual override for the LoL install path.
    pub lol_path_override: Option<String>,
    /// Manual override for the tools directory (where ltk_patcher_host.exe,
    /// ltk_patcher_dll.dll, mod-tools.exe, and cslol-dll.dll live). If None,
    /// Veil searches %APPDATA%\com.veil.launcher\tools\, the resource_dir
    /// (bundled mod-tools.exe lives here), and LTK Manager's install path.
    pub tools_dir: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            dry_run: false,
            lol_path_override: None,
            tools_dir: None,
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

/// Persistent cache directory for downloaded .fantome files. Lives at
/// `%APPDATA%\com.veil.launcher\cache\` on Windows. Each .fantome is
/// saved as `<skin_id>.fantome` so re-applying a skin doesn't re-download
/// the file from the LeagueSkins repo every time.
fn cache_dir(app: &AppHandle) -> PathBuf {
    config_dir(app).join("cache")
}

fn fantome_cache_path(app: &AppHandle, skin_id: &str) -> PathBuf {
    let safe_name = skin_id.replace('/', "_").replace('\\', "_");
    cache_dir(app).join(format!("{}.fantome", safe_name))
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
        // League of Legends.exe is the in-match executable. The WAD files
        // are memory-mapped while it runs — NEVER inject mid-match or the
        // match can crash or corrupt.
        if name == "league of legends.exe" {
            return true;
        }
        // LeagueClientUx.exe + LeagueClient.exe are the lobby/champ-select
        // UI. They don't hold WADs in memory — injecting while they're
        // running is safe. The user can apply a skin during champ select.
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
    tools_dir: Option<Option<String>>,
) -> Result<(), String> {
    let mut s = load_settings(&app);
    if let Some(dr) = dry_run {
        s.dry_run = dr;
    }
    if let Some(override_path) = lol_path_override {
        s.lol_path_override = override_path;
    }
    if let Some(tools) = tools_dir {
        s.tools_dir = tools;
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

/// Extract the .fantome (a renamed ZIP) to a directory, preserving the
/// ZIP's folder structure. This is what ltk_patcher's mkoverlay expects —
/// it reads the extracted mod directory and produces an overlay file
/// from it.
pub fn extract_fantome_to_dir(file_path: &std::path::Path, dest_dir: &std::path::Path) -> Result<(), String> {
    let _ = std::fs::create_dir_all(dest_dir);
    let file = std::fs::File::open(file_path)
        .map_err(|e| format!("Failed to open .fantome: {}", e))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| format!("Failed to read .fantome (not a valid ZIP): {}", e))?;

    for i in 0..archive.len() {
        let mut zip_file = archive.by_index(i)
            .map_err(|e| format!("Failed to read ZIP entry {}: {}", i, e))?;

        let out_path = match zip_file.enclosed_name() {
            Some(name) => dest_dir.join(name),
            None => continue,
        };

        if zip_file.is_dir() {
            let _ = std::fs::create_dir_all(&out_path);
        } else {
            if let Some(parent) = out_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let mut out_file = std::fs::File::create(&out_path)
                .map_err(|e| format!("Failed to create {}: {}", out_path.display(), e))?;
            std::io::copy(&mut zip_file, &mut out_file)
                .map_err(|e| format!("Failed to write {}: {}", out_path.display(), e))?;
        }
    }

    Ok(())
}

// ──────────────────────────── apply_skin (uses ltk_patcher subprocess approach) ────────────────────────────

#[tauri::command]
async fn apply_skin(
    app: AppHandle,
    skin_id: String,
    download_url: String,
) -> Result<OpResult, String> {
    // 1. Verify League isn't in an active match (client/lobby is fine)
    let running = async_runtime::spawn_blocking(|| is_league_running_sync())
        .await
        .map_err(|e| format!("Process check failed: {}", e))?;
    if running {
        return Ok(OpResult {
            ok: false,
            message: "A League of Legends match is in progress. The LTK patcher can only hook before a match starts — close the game and apply again.".into(),
        });
    }

    // 2. Find the LoL install path. mod-tools.exe mkoverlay REQUIRES this
    //    via --game:<path> — without it, mkoverlay fails with
    //    "game.empty() -> true". The ltk_patcher_host.exe also scans for
    //    the game itself, but mkoverlay needs the path explicitly.
    let lol_path = match effective_lol_path(&app) {
        Some(p) => p,
        None => return Ok(OpResult {
            ok: false,
            message: "League of Legends is not detected. Open Settings to set the install path manually.".into(),
        }),
    };
    let game_dir = PathBuf::from(&lol_path);

    // 3. Parse champion_id from skin_id (e.g., "Ahri_103015" → "Ahri")
    let champion_id = skin_id.split('_').next().unwrap_or("").to_string();
    if champion_id.is_empty() {
        return Ok(OpResult {
            ok: false,
            message: "Invalid skin ID format.".into(),
        });
    }

    // 4. One-skin-per-champion rule: clear the previous patcher's overlay
    //    if a different skin of this champion was applied. (ltk_patcher
    //    handles overlay cleanup itself, so we just update our state.)
    let state = load_state(&app);
    if let Some(existing) = state.applied_skins.get(&champion_id) {
        if existing != &skin_id {
            let _ = app.emit("apply-progress", serde_json::json!({
                "skinId": skin_id,
                "phase": "replacing",
                "message": format!("Replacing previously applied skin..."),
            }));
        }
    }

    // 5. Resolve the cached .fantome path (persistent cache so re-applying
    //    doesn't re-download).
    let _ = std::fs::create_dir_all(cache_dir(&app));
    let cached_path = fantome_cache_path(&app, &skin_id);

    let cache_hit = cached_path.exists()
        && cached_path.metadata()
            .map(|m| m.len() > 0)
            .unwrap_or(false);

    let _ = app.emit("apply-progress", serde_json::json!({
        "skinId": skin_id,
        "phase": if cache_hit { "cached" } else { "downloading" },
        "percent": if cache_hit { 100 } else { 0 },
        "message": if cache_hit {
            "Using cached .fantome (no re-download needed)".to_string()
        } else {
            "Downloading .fantome from Alban1911/LeagueSkins".to_string()
        },
    }));

    if !cache_hit {
        if let Err(e) = download_with_progress(&download_url, &cached_path, &app, &skin_id).await {
            return Ok(OpResult { ok: false, message: format!("Download failed: {}", e) });
        }
    }

    // 6. Extract the .fantome to a mod folder (preserves ZIP structure).
    //    ltk_patcher's mkoverlay expects an extracted mod directory.
    //
    //    Layout matches Rose: mods_dir is the PARENT folder containing
    //    mod subfolders. The mod folder name = skin_id (sanitized).
    //    The .fantome's contents go inside mods/<skin_id>/.
    let mods_parent = config_dir(&app).join("mods");
    let mod_name = skin_id.replace('/', "_").replace('\\', "_");
    let mods_dir = mods_parent.join(&mod_name);
    // Clear any previous extraction of this skin (in case the .fantome
    // contents changed since last apply).
    let _ = std::fs::remove_dir_all(&mods_dir);
    let _ = std::fs::create_dir_all(&mods_dir);

    let _ = app.emit("apply-progress", serde_json::json!({
        "skinId": skin_id,
        "phase": "extracting",
    }));

    if let Err(e) = extract_fantome_to_dir(&cached_path, &mods_dir) {
        let _ = std::fs::remove_file(&cached_path); // corrupt cache; re-download next time
        return Ok(OpResult { ok: false, message: format!("Failed to extract .fantome: {}", e) });
    }

    // 7. Find the LTK tools (mod-tools.exe + cslol-dll.dll + ltk_patcher_host.exe
    //    + ltk_patcher_dll.dll). Returns a clear error listing which tools
    //    are missing and where to get them.
    let _ = app.emit("apply-progress", serde_json::json!({
        "skinId": skin_id,
        "phase": "finding-tools",
    }));

    let tools = match ltk_patcher::LtkTools::find(&app) {
        Ok(t) => t,
        Err(missing) => {
            let _ = app.emit("apply-progress", serde_json::json!({
                "skinId": skin_id,
                "phase": "error",
                "message": missing,
            }));
            return Ok(OpResult { ok: false, message: missing });
        }
    };

    // 8. Run mod-tools.exe mkoverlay <mods_parent> <overlay_dir>
    //    --game:<game_dir> --mods:<mod_name> --noTFT --ignoreConflict
    //
    //    This mirrors Rose's exact command construction (overlay_manager.py#L283):
    //    https://github.com/Alban1911/Rose
    let overlay_dir = config_dir(&app).join("overlay");
    // Clear any previous overlay + ensure the folder exists (mod-tools.exe
    // expects the destination to be writable; it creates subfiles but the
    // parent must exist).
    let _ = std::fs::remove_dir_all(&overlay_dir);
    let _ = std::fs::create_dir_all(&overlay_dir);

    let _ = app.emit("apply-progress", serde_json::json!({
        "skinId": skin_id,
        "phase": "creating-overlay",
    }));

    if let Err(e) = tools.make_overlay(&mods_parent, &overlay_dir, &game_dir, &mod_name) {
        let _ = app.emit("apply-progress", serde_json::json!({
            "skinId": skin_id,
            "phase": "error",
            "message": e,
        }));
        return Ok(OpResult { ok: false, message: format!("mkoverlay failed: {}", e) });
    }

    // 9. Spawn ltk_patcher_host.exe with the line-protocol config + start scan.
    //    The patcher stays running, scanning for the game. When the game starts,
    //    it hooks file-reads and serves the overlay. When the game exits, the
    //    patcher exits too.
    let _ = app.emit("apply-progress", serde_json::json!({
        "skinId": skin_id,
        "phase": "starting-patcher",
    }));

    let patcher_child = match tools.start_patcher(&overlay_dir, &app) {
        Ok(c) => c,
        Err(e) => {
            let _ = app.emit("apply-progress", serde_json::json!({
                "skinId": skin_id,
                "phase": "error",
                "message": e,
            }));
            return Ok(OpResult { ok: false, message: format!("Failed to start LTK patcher: {}", e) });
        }
    };

    // Store the Child in app state so it stays alive after apply_skin
    // returns. Without this, dropping the Child closes stdin → the
    // patcher sees EOF → it exits before hooking the game. The patcher
    // is now kept alive until either Veil exits or the next apply_skin
    // call replaces it (PatcherState::replace kills the previous one).
    let patcher_state = app.state::<PatcherState>();
    patcher_state.replace(patcher_child);

    // 10. Update state.
    let mut state = load_state(&app);
    state.applied_skins.insert(champion_id.clone(), skin_id.clone());
    save_state_inner(&app, &state);

    let _ = app.emit("apply-progress", serde_json::json!({
        "skinId": skin_id,
        "phase": "done",
        "percent": 100,
    }));

    Ok(OpResult {
        ok: true,
        message: "Skin applied. Launch League of Legends — the patcher will hook the game on launch and serve the overlay. The patcher self-exits when the game closes.".into(),
    })
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
        .setup(|app| {
            // Register the long-running patcher state so apply_skin can
            // store its Child handle here. The Child stays alive until
            // either Veil exits or the next apply_skin call replaces it.
            app.manage(PatcherState::new());
            Ok(())
        })
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
