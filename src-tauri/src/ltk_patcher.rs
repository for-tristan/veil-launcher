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

        // 1. Veil's bundled resource directory (patcher_host.exe + patcher_dll.dll
        //    + mod-tools.exe all live here after install).
        if let Ok(res_dir) = app.path().resource_dir() {
            search_paths.push(res_dir);
        }

        // 2. User-configurable tools directory from settings.
        if let Some(s) = std::fs::read_to_string(
            app.path().app_config_dir().unwrap_or_default().join("settings.json")
        ).ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        {
            if let Some(tools_dir) = s.get("toolsDir").and_then(|v| v.as_str()) {
                search_paths.push(PathBuf::from(tools_dir));
            }
        }

        // 3. %APPDATA%\com.veil.launcher\tools\
        if let Ok(app_config) = app.path().app_config_dir() {
            search_paths.push(app_config.join("tools"));
        }

        // 4. Fallback: LTK Manager's default install paths (for users who
        //    installed LTK Manager and want to use those binaries directly
        //    without copying).
        if let Some(prog_files) = std::env::var_os("PROGRAMFILES") {
            search_paths.push(PathBuf::from(&prog_files).join("LTK Manager"));
        }
        if let Some(local_app) = std::env::var_os("LOCALAPPDATA") {
            search_paths.push(PathBuf::from(&local_app).join("LTK Manager"));
            search_paths.push(PathBuf::from(&local_app).join("Programs").join("LTK Manager"));
        }

        // 5. Rose install paths (for users who have Rose — provides mod-tools.exe
        //    + cslol-dll.dll in Rose's injection/tools/ folder).
        if let Some(prog_files) = std::env::var_os("PROGRAMFILES") {
            search_paths.push(PathBuf::from(&prog_files).join("Rose").join("injection").join("tools"));
        }
        if let Some(local_app) = std::env::var_os("LOCALAPPDATA") {
            search_paths.push(PathBuf::from(&local_app).join("Rose").join("injection").join("tools"));
        }

        // Look for Veil's renamed bundled names FIRST (patcher_host.exe +
        // patcher_dll.dll). Fall back to the original LTK Manager names
        // (ltk_patcher_host.exe + ltk_patcher_dll.dll) for users who have
        // LTK Manager installed.
        let patcher_host = find_in_paths("patcher_host.exe", &search_paths)
            .or_else(|| find_in_paths("ltk_patcher_host.exe", &search_paths));
        let patcher_dll = find_in_paths("patcher_dll.dll", &search_paths)
            .or_else(|| find_in_paths("ltk_patcher_dll.dll", &search_paths));

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
            missing.push("patcher_host.exe (bundled with Veil since v0.1.1; if missing, reinstall Veil)");
        }
        if patcher_dll.is_none() {
            missing.push("patcher_dll.dll (bundled with Veil since v0.1.1; if missing, reinstall Veil)");
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
                 - Reinstall Veil to get patcher_host.exe + patcher_dll.dll + mod-tools.exe back.\n\
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

    /// Spawn patcher_host.exe and configure it via stdin (same line protocol
    /// Rose uses for ltk_patcher_host.exe — the binary is API-compatible
    /// regardless of the filename).
    ///
    /// The patcher stays alive, scanning for the game. When the game starts,
    /// it hooks file-reads and serves the overlay. When the game exits, the
    /// patcher exits too.
    pub fn start_patcher(&self, overlay_dir: &Path) -> Result<Child, String> {
        let prefix = format!("{}\\",
            overlay_dir.to_string_lossy().trim_end_matches('\\').trim_end_matches('/'));

        let mut child = Command::new(&self.patcher_host)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to spawn patcher_host.exe: {}", e))?;

        // Send the line-protocol config + start commands to stdin
        if let Some(stdin) = child.stdin.as_mut() {
            let _ = writeln!(stdin, "config loglevel 0");
            let _ = writeln!(stdin, "config flags 0");
            let _ = writeln!(stdin, "config prefix {}", prefix);
            let _ = writeln!(stdin, "start scan");
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
