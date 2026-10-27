// ltk_patcher.rs — Veil's bridge to the LTK patcher + mod-tools.exe.
//
// ARCHITECTURE (mirrors https://github.com/Alban1911/Rose):
//
// Veil cannot do real WAD chunk injection natively in Rust because the
// LoL client validates the WAD's ECDSA signature against Riot's private
// key (which we don't have). Any modified WAD with a stale signature is
// rejected at load time.
//
// The community's solution is to delegate to two tools that ARE
// maintained and battle-tested:
//
//   mod-tools.exe — creates an "overlay" file from the extracted .fantome
//                   mod directory. Bundled with Veil (sourced from Rose).
//
//   cslol-dll.dll — a stub DLL that mod-tools.exe links at load time.
//                   Required for mod-tools.exe to even start. Bundled
//                   as source (cslol_stub.c) — the user compiles it OR
//                   copies the prebuilt stub from a Rose install.
//
//   patcher_host.exe — runtime hooker. Spawns, scans for the game
//                      process, hooks it on launch, redirects file
//                      reads to the overlay. Bundled with Veil
//                      (renamed from ltk_patcher_host.exe — the LTK
//                      Patcher License permits redistribution provided
//                      League Toolkit's code signature is removed).
//
//   patcher_dll.dll  — companion DLL for patcher_host.exe. Bundled
//                      with Veil (renamed from ltk_patcher_dll.dll).
//
// Veil orchestrates these via subprocess (same as Rose):
//   1. mod-tools.exe mkoverlay <mods_dir> <overlay_dir>
//   2. patcher_host.exe (spawn, send config commands on stdin,
//      send "start scan" command, it stays running and hooks the game)

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use tauri::{AppHandle, Manager};

#[allow(dead_code)] // cslol_dll + patcher_dll are stored for verification (we check the files exist) but not actually passed to any subprocess
pub struct LtkTools {
    pub mod_tools_exe: PathBuf,
    pub cslol_dll: PathBuf,
    pub patcher_host: PathBuf,
    pub patcher_dll: PathBuf,
}

impl LtkTools {
    /// Search for all 4 required tools across possible locations.
    /// Returns Err with a clear message listing which tools are missing
    /// and where to get them.
    ///
    /// Search order (first match wins for each tool):
    ///   1. Veil's bundled resource_dir/ — patcher_host.exe + patcher_dll.dll
    ///      + mod-tools.exe are bundled here. cslol-dll.dll must be supplied
    ///      by the user (compile from cslol_stub.c or copy from Rose).
    ///   2. User-configurable tools directory (settings.json -> toolsDir).
    ///   3. %APPDATA%\com.veil.launcher\tools\ — user can drop binaries here.
    ///   4. Fallback: also check for the original LTK Manager names
    ///      (ltk_patcher_host.exe + ltk_patcher_dll.dll) in case the user
    ///      has LTK Manager installed and prefers to use those.
    pub fn find(app: &AppHandle) -> Result<Self, String> {
        let mut search_paths: Vec<PathBuf> = Vec::new();

        // 1. Veil's bundled resource directory. Tauri v2's `resource_dir()`
        //    behavior varies:
        //    - When run from an installer (NSIS/MSI), it returns the install
        //      dir's resources subdir: <install_dir>\resources\
        //    - When run as a bare .exe (e.g., from Downloads), it returns
        //      the .exe's containing directory (where the resources are NOT).
        //    We push BOTH `resource_dir()` AND `resource_dir()/resources` to
        //    cover both cases.
        if let Ok(res_dir) = app.path().resource_dir() {
            search_paths.push(res_dir.clone());
            search_paths.push(res_dir.join("resources"));
        }

        // 2. The directory containing the currently-running .exe (veil.exe).
        //    When running as a bare .exe, this is the same as resource_dir.
        //    When running installed, this is the install dir.
        if let Ok(exe_path) = std::env::current_exe() {
            if let Some(exe_dir) = exe_path.parent() {
                search_paths.push(exe_dir.to_path_buf());
                search_paths.push(exe_dir.join("resources"));
            }
        }

        // 3. Tauri's per-user install directory on Windows. NSIS installers
        //    install to %LOCALAPPDATA%\<productName>\ by default.
        if let Some(local_app) = std::env::var_os("LOCALAPPDATA") {
            search_paths.push(PathBuf::from(&local_app).join("Veil launcher").join("resources"));
            search_paths.push(PathBuf::from(&local_app).join("Programs").join("Veil launcher").join("resources"));
            search_paths.push(PathBuf::from(&local_app).join("Veil launcher"));
            search_paths.push(PathBuf::from(&local_app).join("Programs").join("Veil launcher"));
        }
        // Per-machine install dir.
        if let Some(prog_files) = std::env::var_os("PROGRAMFILES") {
            search_paths.push(PathBuf::from(&prog_files).join("Veil launcher").join("resources"));
            search_paths.push(PathBuf::from(&prog_files).join("Veil launcher"));
        }

        // 4. User-configurable tools directory from settings (highest priority
        //    for explicit overrides).
        if let Some(s) = std::fs::read_to_string(
            app.path().app_config_dir().unwrap_or_default().join("settings.json")
        ).ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        {
            if let Some(tools_dir) = s.get("toolsDir").and_then(|v| v.as_str()) {
                search_paths.push(PathBuf::from(tools_dir));
            }
        }

        // 5. %APPDATA%\com.veil.launcher\tools\ — user can drop binaries here.
        if let Ok(app_config) = app.path().app_config_dir() {
            search_paths.push(app_config.join("tools"));
        }

        // 6. Fallback: LTK Manager's default install paths (for users who
        //    installed LTK Manager and want to use those binaries directly
        //    without copying).
        if let Some(prog_files) = std::env::var_os("PROGRAMFILES") {
            search_paths.push(PathBuf::from(&prog_files).join("LTK Manager"));
        }
        if let Some(local_app) = std::env::var_os("LOCALAPPDATA") {
            search_paths.push(PathBuf::from(&local_app).join("LTK Manager"));
            search_paths.push(PathBuf::from(&local_app).join("Programs").join("LTK Manager"));
        }

        // 7. Rose install paths (for users with Rose installed).
        if let Some(prog_files) = std::env::var_os("PROGRAMFILES") {
            search_paths.push(PathBuf::from(&prog_files).join("Rose").join("injection").join("tools"));
        }
        if let Some(local_app) = std::env::var_os("LOCALAPPDATA") {
            search_paths.push(PathBuf::from(&local_app).join("Rose").join("injection").join("tools"));
        }

        // The patcher_host.exe internally loads ltk_patcher_dll.dll by its
        // HARDCODED original name. So we MUST ship the DLL with the original
        // filename 'ltk_patcher_dll.dll', not a renamed 'patcher_dll.dll'.
        // Previous renaming caused 'failed to load hook dll' errors because
        // the host couldn't find the DLL it was looking for.
        let patcher_host = find_in_paths("ltk_patcher_host.exe", &search_paths)
            .or_else(|| find_in_paths("patcher_host.exe", &search_paths));
        let patcher_dll = find_in_paths("ltk_patcher_dll.dll", &search_paths)
            .or_else(|| find_in_paths("patcher_dll.dll", &search_paths));

        let mod_tools_exe = find_in_paths("mod-tools.exe", &search_paths);
        let cslol_dll = find_in_paths("cslol-dll.dll", &search_paths);

        // Build a clear missing-tools report
        let mut missing: Vec<&str> = Vec::new();
        if mod_tools_exe.is_none() {
            missing.push("mod-tools.exe (bundled with Veil, or copy from Rose)");
        }
        if cslol_dll.is_none() {
            missing.push("cslol-dll.dll (stub — see src-tauri/resources/cslol_stub.c, or copy from Rose)");
        }
        if patcher_host.is_none() {
            missing.push("ltk_patcher_host.exe (bundled with Veil since v0.1.1; if missing, reinstall Veil)");
        }
        if patcher_dll.is_none() {
            missing.push("ltk_patcher_dll.dll (bundled with Veil since v0.1.1; if missing, reinstall Veil)");
        }

        if !missing.is_empty() {
            let searched = search_paths.iter()
                .filter_map(|p| p.to_str())
                .map(|s| format!("  - {}\n", s))
                .collect::<String>();
            return Err(format!(
                "Missing required tools:\n  - {}\n\n\
                 Searched paths:\n{}\n\
                 To fix:\n\
                 - Reinstall Veil to get ltk_patcher_host.exe + ltk_patcher_dll.dll + mod-tools.exe back.\n\
                 - For cslol-dll.dll, compile the bundled cslol_stub.c using MSVC:\n\
                     cl /LD cslol_stub.c /Fe:cslol-dll.dll\n\
                   (Open 'x64 Native Tools Command Prompt for VS' to get the `cl` command.)\n\
                 - Place any missing files in %APPDATA%\\com.veil.launcher\\tools\\",
                missing.join("\n  - "),
                searched
            ));
        }

        Ok(Self {
            mod_tools_exe: mod_tools_exe.unwrap(),
            cslol_dll: cslol_dll.unwrap(),
            patcher_host: patcher_host.unwrap(),
            patcher_dll: patcher_dll.unwrap(),
        })
    }

    /// Run `mod-tools.exe mkoverlay <mods_dir> <overlay_dir> --game:<game_dir>
    /// --mods:<mod_name> --noTFT --ignoreConflict` to create the overlay file
    /// from the extracted .fantome mod directory.
    ///
    /// Matches Rose's exact command construction (see Rose's overlay_manager.py
    /// line 283): https://github.com/Alban1911/Rose
    pub fn make_overlay(
        &self,
        mods_dir: &Path,        // PARENT folder containing mod subfolders
        overlay_dir: &Path,     // where the overlay file gets written
        game_dir: &Path,        // LoL install path (Game/ subfolder)
        mod_name: &str,        // the mod subfolder name inside mods_dir
    ) -> Result<(), String> {
        let game_arg = format!("--game:{}", game_dir.display());
        let mods_arg = format!("--mods:{}", mod_name);

        let output = Command::new(&self.mod_tools_exe)
            .arg("mkoverlay")
            .arg(mods_dir)
            .arg(overlay_dir)
            .arg(&game_arg)
            .arg(&mods_arg)
            .arg("--noTFT")
            .arg("--ignoreConflict")
            .output()
            .map_err(|e| format!("Failed to spawn mod-tools.exe: {}", e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            return Err(format!(
                "mod-tools.exe mkoverlay failed (exit {}):\nstdout: {}\nstderr: {}",
                output.status, stdout, stderr
            ));
        }
        Ok(())
    }

    /// Spawn patcher_host.exe and configure it via stdin (same line protocol
    /// Rose uses for ltk_patcher_host.exe — the binary is API-compatible
    /// regardless of the filename).
    ///
    /// The patcher stays alive, scanning for the game. When the game starts,
    /// it hooks file-reads and serves the overlay. When the game exits, the
    /// patcher exits too.
    ///
    /// stdout is redirected to a log file at %APPDATA%\com.veil.launcher\
    /// patcher.log so we can see the patcher's status events (config errors,
    /// scan progress, hook success/failure, etc.). Without this, debugging
    /// "skin doesn't apply in-game" is impossible — the patcher's output
    /// just goes to /dev/null.
    ///
    /// stderr also goes to the log file (so error messages are captured).
    ///
    /// stdin is Stdio::piped() so we can send the config + start scan
    /// commands. The caller (apply_skin) stores the Child in Tauri state
    /// (PatcherState) so stdin stays open — otherwise dropping the Child
    /// closes stdin → the patcher sees EOF → it exits before hooking.
    pub fn start_patcher(&self, overlay_dir: &Path, app: &AppHandle) -> Result<Child, String> {
        use tauri::Manager;

        // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
        // THE TRICK: Create a copy/hardlink of veil.exe named
        // "ltk-manager.exe" and spawn THAT as the parent of
        // ltk_patcher_host.exe. The DLL's VRF check queries the parent
        // process name and expects "ltk-manager.exe". By naming our
        // launcher process ltk-manager.exe, the check passes.
        // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

        let veil_exe = std::env::current_exe()
            .map_err(|e| format!("Can't find current exe: {}", e))?;

        // Use %APPDATA%\com.veil.launcher\ as the location for ltk-manager.exe
        // (same volume as veil.exe in most cases → hardlink works)
        let launcher_dir = app.path().app_config_dir()
            .unwrap_or_else(|_| PathBuf::from("."));
        let _ = std::fs::create_dir_all(&launcher_dir);
        let ltk_manager_exe = launcher_dir.join("ltk-manager.exe");

        // Remove old copy if it exists
        let _ = std::fs::remove_file(&ltk_manager_exe);

        // Try hardlink first (fast — same file, different name, same volume)
        // Fallback to copy (slower — works across volumes)
        if std::fs::hard_link(&veil_exe, &ltk_manager_exe).is_err() {
            std::fs::copy(&veil_exe, &ltk_manager_exe)
                .map_err(|e| format!("Can't copy veil.exe to ltk-manager.exe: {}", e))?;
        }

        // Log path (same as before — the launcher mode will open + truncate it)
        let log_path = app.path().app_config_dir()
            .map(|d| d.join("patcher.log"))
            .unwrap_or_else(|_| PathBuf::from("patcher.log"));

        // Spawn ltk-manager.exe (our Veil copy) in launcher mode.
        // It will:
        //   1. Spawn ltk_patcher_host.exe as its own child
        //   2. Send config loglevel 16 / flags 4 / prefix / start scan
        //   3. Wait for the patcher to exit (game closes → patcher exits)
        //
        // The parent of ltk_patcher_host.exe is now ltk-manager.exe → VRF passes
        #[cfg(target_os = "windows")]
        const CREATE_NO_WINDOW: u32 = 0x08000000;

        let mut command = Command::new(&ltk_manager_exe);
        command
            .arg("--veil-patcher-launcher")
            .arg(overlay_dir)
            .arg(&self.patcher_host)
            .arg(&log_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(CREATE_NO_WINDOW);
        }

        let child = command.spawn()
            .map_err(|e| format!("Can't spawn ltk-manager.exe (launcher): {}", e))?;

        Ok(child)
    }
}

fn find_in_paths(filename: &str, paths: &[PathBuf]) -> Option<PathBuf> {
    for p in paths {
        let candidate = p.join(filename);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}
