// ltk_patcher.rs — Veil's bridge to the LTK patcher + mod-tools.exe.
//
// ARCHITECTURE (mirrors https://github.com/Alban1911/Rose):
//
// Veil cannot do real WAD chunk injection natively in Rust because:
//   - The LoL client validates the WAD's ECDSA signature against Riot's
//     private key (which we don't have). Any modified WAD with a stale
//     signature is rejected at load time.
//   - Implementing runtime hooking (the alternative) is a separate
//     massive project.
//
// The community's solution is to delegate to two tools that ARE
// maintained and battle-tested:
//
//   mod-tools.exe — creates an "overlay" file from the extracted .fantome
//                   mod directory. Bundled with Veil (sourced from Rose).
//
//   cslol-dll.dll — a stub DLL that mod-tools.exe links at load time.
//                   Required for mod-tools.exe to even start. User-provided
//                   (compile from src-tauri/resources/cslol_stub.c via MSVC
//                   or copy from a Rose install).
//
//   ltk_patcher_host.exe — runtime hooker. Spawns, scans for the game
//                          process, hooks it on launch, redirects file
//                          reads to the overlay. User-provided from LTK
//                          Manager (https://github.com/LeagueToolkit/ltk-manager).
//
//   ltk_patcher_dll.dll  — companion DLL for ltk_patcher_host.exe.
//                          User-provided from LTK Manager.
//
// Veil orchestrates these via subprocess (same as Rose):
//   1. mod-tools.exe mkoverlay <mods_dir> <overlay_dir>
//   2. ltk_patcher_host.exe (spawn, send config commands on stdin,
//      send "start scan" command, it stays running and hooks the game)

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use tauri::{AppHandle, Manager};

pub struct LtkTools {
    pub mod_tools_exe: PathBuf,
    pub cslol_dll: PathBuf,
    pub ltk_patcher_host: PathBuf,
    pub ltk_patcher_dll: PathBuf,
}

impl LtkTools {
    /// Search for all 4 required tools across possible locations.
    /// Returns Err with a clear message listing which tools are missing
    /// and where to get them.
    pub fn find(app: &AppHandle) -> Result<Self, String> {
        // Search order:
        //   1. User-configurable tools directory (settings.json -> tools_dir)
        //   2. %APPDATA%\com.veil.launcher\tools\
        //   3. Veil's own install directory (resource_dir + /tools/)
        //   4. LTK Manager's default install path
        //   5. Rose's default install path (for mod-tools.exe + cslol-dll.dll)

        let mut search_paths: Vec<PathBuf> = Vec::new();

        // User-configurable tools dir from settings
        if let Some(s) = std::fs::read_to_string(app.path().app_config_dir().unwrap_or_default().join("settings.json")).ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        {
            if let Some(tools_dir) = s.get("toolsDir").and_then(|v| v.as_str()) {
                search_paths.push(PathBuf::from(tools_dir));
            }
        }

        // %APPDATA%\com.veil.launcher\tools\
        if let Ok(app_config) = app.path().app_config_dir() {
            search_paths.push(app_config.join("tools"));
        }

        // Veil's resource directory (bundled mod-tools.exe lives here)
        if let Ok(res_dir) = app.path().resource_dir() {
            search_paths.push(res_dir.join("tools"));
        }

        // LTK Manager's default install path on Windows
        if let Some(prog_files) = std::env::var_os("PROGRAMFILES") {
            search_paths.push(PathBuf::from(&prog_files).join("LTK Manager"));
        }
        if let Some(local_app) = std::env::var_os("LOCALAPPDATA") {
            search_paths.push(PathBuf::from(&local_app).join("LTK Manager"));
            search_paths.push(PathBuf::from(&local_app).join("Programs").join("LTK Manager"));
        }

        // Rose install path (provides mod-tools.exe + cslol-dll.dll)
        if let Some(prog_files) = std::env::var_os("PROGRAMFILES") {
            search_paths.push(PathBuf::from(&prog_files).join("Rose").join("injection").join("tools"));
        }
        if let Some(local_app) = std::env::var_os("LOCALAPPDATA") {
            search_paths.push(PathBuf::from(&local_app).join("Rose").join("injection").join("tools"));
        }

        // Find each tool
        let mod_tools_exe = find_in_paths("mod-tools.exe", &search_paths);
        let cslol_dll = find_in_paths("cslol-dll.dll", &search_paths);
        let ltk_patcher_host = find_in_paths("ltk_patcher_host.exe", &search_paths);
        let ltk_patcher_dll = find_in_paths("ltk_patcher_dll.dll", &search_paths);

        // Build a clear missing-tools report
        let mut missing: Vec<&str> = Vec::new();
        if mod_tools_exe.is_none() { missing.push("mod-tools.exe (bundled with Veil, or copy from Rose)"); }
        if cslol_dll.is_none() { missing.push("cslol-dll.dll (stub — see src-tauri/resources/cslol_stub.c, or copy from Rose)"); }
        if ltk_patcher_host.is_none() { missing.push("ltk_patcher_host.exe (install LTK Manager: https://github.com/LeagueToolkit/ltk-manager/releases)"); }
        if ltk_patcher_dll.is_none() { missing.push("ltk_patcher_dll.dll (install LTK Manager: https://github.com/LeagueToolkit/ltk-manager/releases)"); }

        if !missing.is_empty() {
            return Err(format!(
                "Missing required tools:\n  - {}\n\n\
                 Place the missing files in one of these locations:\n  - %APPDATA%\\com.veil.launcher\\tools\\\n  - Next to veil.exe (Veil's install directory)\n  - A custom path (set in Settings → Tools directory)\n\n\
                 Searched paths:\n  - {}",
                missing.join("\n  - "),
                search_paths.iter()
                    .filter_map(|p| p.to_str())
                    .map(|s| format!("{}\n", s))
                    .collect::<String>()
            ));
        }

        Ok(Self {
            mod_tools_exe: mod_tools_exe.unwrap(),
            cslol_dll: cslol_dll.unwrap(),
            ltk_patcher_host: ltk_patcher_host.unwrap(),
            ltk_patcher_dll: ltk_patcher_dll.unwrap(),
        })
    }

    /// Run `mod-tools.exe mkoverlay <mods_dir> <overlay_dir>` to create
    /// the overlay file from the extracted .fantome mod directory.
    pub fn make_overlay(&self, mods_dir: &Path, overlay_dir: &Path) -> Result<(), String> {
        let output = Command::new(&self.mod_tools_exe)
            .arg("mkoverlay")
            .arg(mods_dir)
            .arg(overlay_dir)
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

    /// Spawn ltk_patcher_host.exe and configure it via stdin (same line
    /// protocol Rose uses):
    ///   config loglevel <level>
    ///   config flags <flags>
    ///   config prefix <overlay_dir>\
    ///   start scan
    ///
    /// The patcher stays alive, scanning for the game process. When the
    /// game starts, it hooks file-reads and serves the overlay. When the
    /// game exits, the patcher exits too.
    pub fn start_patcher(&self, overlay_dir: &Path) -> Result<Child, String> {
        let prefix = format!("{}\\",
            overlay_dir.to_string_lossy().trim_end_matches('\\').trim_end_matches('/'));

        let mut child = Command::new(&self.ltk_patcher_host)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to spawn ltk_patcher_host.exe: {}", e))?;

        // Send the line-protocol config + start commands to stdin
        if let Some(stdin) = child.stdin.as_mut() {
            let _ = writeln!(stdin, "config loglevel 0");
            let _ = writeln!(stdin, "config flags 0");
            let _ = writeln!(stdin, "config prefix {}", prefix);
            let _ = writeln!(stdin, "start scan");
            // Close stdin so the patcher knows we're done sending commands
            // (Rose keeps stdin open for status updates; we just close it
            // and let the patcher do its thing — it'll exit on its own
            // when the game closes).
        }

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
