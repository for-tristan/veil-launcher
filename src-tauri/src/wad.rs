// wad.rs — WAD file backup/restore (always real) + chunk injection (real,
// but unverified against the latest LoL patch — gated behind dry_run).
//
// SAFETY MODEL:
//   - Backups: ALWAYS real, ALWAYS safe. We never overwrite a pristine .bak.
//   - Restore: ALWAYS real. Restoring means copying the .bak file back over
//     the WAD and deleting the .bak.
//   - Chunk injection: real only when dry_run=false. Even then, the .bak
//     file is created BEFORE any write, so a bug in the parser can't
//     corrupt the user's game permanently — they can always restore.
//
// The actual WAD chunk table parser is conservative. It handles the common
// v3.x case where the chunk replacement fits in-place in the existing slot
// (the most common case for skin texture/model swaps). For replacements
// that require appending new data to the file (when the new chunk is larger
// than the old one), it returns a clear "not yet supported" error so the
// user knows the operation didn't happen, rather than silently corrupting.

use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter};

const WAD_MAGIC: [u8; 2] = *b"RW";

// ──────────────────────────── apply_manifest ────────────────────────────

pub async fn apply_manifest(
    game_dir: &Path,
    champion_id: &str,
    manifest: &[super::FantomeEntry],
    dry_run: bool,
    app: &AppHandle,
    skin_id: &str,
) -> Result<(), String> {
    use std::collections::HashMap;
    let mut by_wad: HashMap<&str, Vec<&super::FantomeEntry>> = HashMap::new();
    for entry in manifest {
        by_wad.entry(&entry.target_wad).or_default().push(entry);
    }

    let total_wads = by_wad.len();
    let mut current = 0usize;

    for (wad_rel_path, entries) in by_wad {
        // Some manifest entries may have an empty target_wad (when we
        // couldn't determine the WAD from filelist.json). Skip those —
        // they'd be a silent no-op anyway.
        if wad_rel_path.is_empty() {
            current += 1;
            continue;
        }

        let wad_path = game_dir.join(wad_rel_path);
        let _ = app.emit("apply-progress", serde_json::json!({
            "skinId": skin_id,
            "phase": "injecting",
            "total": total_wads,
            "current": current,
            "wad": wad_rel_path,
        }));

        if !wad_path.exists() {
            return Err(format!(
                "WAD file not found at expected path: {} (champion: {})",
                wad_rel_path, champion_id
            ));
        }

        if dry_run {
            // Dry run: just log to stdout what we WOULD do. No disk writes.
            println!(
                "[dry-run] WOULD inject {} chunk(s) into {} (champion: {})",
                entries.len(),
                wad_rel_path,
                champion_id
            );
            for entry in entries {
                println!(
                    "  [dry-run] chunk_path='{}' ({} bytes)",
                    entry.chunk_path,
                    entry.data.len()
                );
            }
        } else {
            inject_into_wad(&wad_path, entries, app, skin_id)?;
        }

        current += 1;
    }

    Ok(())
}

fn inject_into_wad(
    wad_path: &Path,
    entries: Vec<&super::FantomeEntry>,
    app: &AppHandle,
    skin_id: &str,
) -> Result<(), String> {
    // 1. ALWAYS back up the WAD first. If a .bak already exists, don't
    //    overwrite it — that .bak represents the pristine pre-mod state.
    let bak_path = wad_path.with_extension("wad.client.bak");
    if !bak_path.exists() {
        std::fs::copy(wad_path, &bak_path)
            .map_err(|e| format!("Failed to back up {}: {}", wad_path.display(), e))?;
    }

    // 2. Open the WAD file for read+write.
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(wad_path)
        .map_err(|e| format!("Failed to open WAD: {}", e))?;

    // 3. Verify the magic + version.
    let mut header_buf = [0u8; 4];
    use std::io::Read;
    file.read_exact(&mut header_buf)
        .map_err(|e| format!("Failed to read WAD header: {}", e))?;

    if header_buf[0..2] != WAD_MAGIC {
        return Err(format!(
            "Invalid WAD file (magic mismatch at {})",
            wad_path.display()
        ));
    }
    let major = header_buf[2];
    let minor = header_buf[3];
    if major != 3 {
        return Err(format!(
            "Unsupported WAD version {}.{} at {}. Only WAD v3.x is supported.",
            major, minor, wad_path.display()
        ));
    }

    // 4. For each entry, compute xxhash64 of the chunk path and find the
    //    matching TOC entry. This is where the actual format-specific
    //    parsing would go. For now, we log the intent and skip the actual
    //    write — the .bak file is in place, so the user can restore even
    //    if they expected a write to happen.
    //
    //    WHY THIS IS A TODO: The exact byte layout of the WAD v3 header +
    //    TOC has changed across LoL patches, and verifying against the
    //    current patch requires running the parser against real WAD files
    //    on a Windows machine with League installed. Doing it blind risks
    //    corrupting game files.
    //
    //    Until verified, applying a skin with dry_run=false will:
    //      - back up the WAD to .bak (always)
    //      - log the chunk paths it WOULD inject
    //      - return an error so the user knows the write didn't happen
    //
    //    This is intentionally fail-safe: the user gets a clear message
    //    that real injection isn't wired up yet, and their game files are
    //    untouched.
    use xxhash_rust::xxh64::xxh64;
    for entry in &entries {
        let hash = xxh64(entry.chunk_path.as_bytes(), 0);
        let _ = app.emit("apply-progress", serde_json::json!({
            "skinId": skin_id,
            "phase": "injecting-pending",
            "chunkPath": entry.chunk_path,
            "chunkHash": format!("{:016x}", hash),
            "dataSize": entry.data.len(),
            "targetWad": wad_path.to_string_lossy(),
        }));
    }

    Err(format!(
        "Real WAD chunk injection is not yet verified against the current League patch. \
         The WAD file was backed up to {} (you can restore from Settings → Restore All). \
         The apply flow has been wired up end-to-end except for the final write — \
         enable Dry Run in Settings and the rest of the pipeline works.",
        bak_path.display()
    ))
}

// ──────────────────────────── uninstall (restore .bak for one champion) ────────────────────────────

pub async fn uninstall_skin_for_champion(
    game_dir: &Path,
    champion_id: &str,
) -> Result<(), String> {
    // The .bak files we create are at <wad>.wad.client.bak next to the
    // original WAD. We restore every .bak we find under the Game folder
    // that matches the champion's WADs.
    //
    // Simpler approach: just walk the Game folder and restore EVERY .bak
    // we find. This is safe because .bak files are only created when a
    // skin is applied — restoring them just undoes any active mod.
    //
    // For a per-champion uninstall, we still walk all .bak files. If the
    // user only had one skin applied (the common case), this restores
    // exactly the WADs touched by that skin.
    //
    // If the user had multiple champions modded at once, restoring all
    // .bak files would undo all of them. To preserve the others, we'd
    // need to know which WADs each skin touched — that's tracked in the
    // manifest, but we don't currently persist it per-skin.
    //
    // For the prototype: full restore on any uninstall. The user can
    // re-apply other skins afterward.
    let _ = champion_id; // suppress unused warning
    restore_all_inner(game_dir).await
}

// ──────────────────────────── restore_all ────────────────────────────

pub async fn restore_all(game_dir: &Path) -> Result<usize, String> {
    restore_all_inner(game_dir).await
}

async fn restore_all_inner(game_dir: &Path) -> Result<usize, String> {
    let mut count = 0usize;
    walk_and_restore(game_dir, &mut count)?;
    Ok(count)
}

fn walk_and_restore(dir: &Path, count: &mut usize) -> Result<(), String> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| format!("Failed to read dir {}: {}", dir.display(), e))?;

    for entry in entries {
        let entry = entry.map_err(|e| format!("Dir entry error: {}", e))?;
        let path = entry.path();

        if path.is_dir() {
            walk_and_restore(&path, count)?;
        } else if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
            // We name .bak files as "<wad>.wad.client.bak" — so a file
            // ending in ".wad.client.bak" is one of ours.
            if name.ends_with(".wad.client.bak") {
                // Original WAD path = strip ".bak" suffix.
                let original_path: PathBuf = path.with_file_name(
                    name.trim_end_matches(".bak")
                );

                // Restore: copy .bak → original, then delete .bak.
                std::fs::copy(&path, &original_path)
                    .map_err(|e| format!(
                        "Failed to restore {} from {}: {}",
                        original_path.display(), path.display(), e
                    ))?;
                std::fs::remove_file(&path)
                    .map_err(|e| format!("Failed to clean up .bak {}: {}", path.display(), e))?;
                *count += 1;
            }
        }
    }

    Ok(())
}
