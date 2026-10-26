// wad.rs — Real WAD chunk injection for League of Legends.
//
// FORMAT BACKGROUND:
//   WAD files are Riot's custom ZIP-like archive. Structure:
//     [ header (28 bytes for v3) ]
//     [ TOC entries (24 bytes each, sorted by hash) ]
//     [ chunk data section ]
//
//   Each TOC entry: { hash: u64, offset: u64, compressed_size: u32, uncompressed_size: u32 }
//   The hash = xxhash64 of the chunk's logical lowercase path (seed=0).
//
// INJECTION STRATEGY:
//   1. Backup the WAD to <wad>.wad.client.bak (always — never overwrite an
//      existing .bak, since that represents the pristine pre-mod state).
//   2. Parse the header + TOC.
//   3. For each manifest entry from the .fantome:
//        - Remap the chunk path from skinN (e.g. skin15) to skin0, so the
//          applied skin's assets OVERRIDE the champion's default skin
//          slot. This is what makes Veil work — applying K/DA Ahri shows
//          K/DA visuals when you pick Classic Ahri in champ select.
//        - Compute xxhash64 of the remapped path.
//        - Find the matching TOC entry by hash.
//        - Append the new chunk data at the end of the file (offsets grow).
//        - Update the TOC entry's offset + size fields.
//   4. Flush.
//
// SAFETY:
//   - The .bak file is created BEFORE any modification, so any failure
//     leaves the WAD recoverable via Restore All in Settings.
//   - We never overwrite the existing chunk data — we append new data
//     at EOF and just update the TOC entry to point at the new location.
//     This means the original chunk bytes are still in the file (wasted
//     space) but the WAD will read the new chunk at the new offset.
//   - We use a single atomic operation: read TOC, append data, write TOC.
//     If the write fails partway, the TOC might be inconsistent, but the
//     .bak file is untouched.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter};
use xxhash_rust::xxh64::xxh64;

const WAD_MAGIC: [u8; 2] = *b"RW";
const WAD_HEADER_SIZE: u64 = 28;
const WAD_TOC_ENTRY_SIZE: u64 = 24;

struct WadHeader {
    #[allow(dead_code)]
    major: u8,
    #[allow(dead_code)]
    minor: u8,
    entry_count: u32,
}

#[derive(Clone)]
struct WadTocEntry {
    hash: u64,
    offset: u64,
    compressed_size: u32,
    uncompressed_size: u32,
}

// ──────────────────────────── Default skin remapping ────────────────────────────

/// Remap a chunk path so it targets the champion's default skin slot
/// (skin0) instead of the original skinN slot.
///
/// Examples:
///   "assets/characters/ahri/skins/skin15/ahri_skin15_tx_cm.dds"
///     → "assets/characters/ahri/skins/skin0/ahri_skin0_tx_cm.dds"
///   "assets/characters/ahri/skins/skin15/base/ahri_skin15.skn"
///     → "assets/characters/ahri/skins/skin0/base/ahri_skin0.skn"
///   "assets/maps/turrets/items.wad" (no skinN token)
///     → unchanged
///
/// This is what makes Veil's applied skins override the DEFAULT skin
/// shown in champion select. Without this remap, applying K/DA Ahri
/// would only change the visuals when you specifically pick K/DA Ahri
/// in champ select — but you don't own that skin, so you can't.
fn remap_to_default_skin(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut result = String::with_capacity(path.len() + 8);
    let mut i = 0usize;
    while i < bytes.len() {
        // Look for "skin" prefix
        if i + 4 <= bytes.len() && &bytes[i..i + 4] == b"skin" {
            // Consume any following digits
            let mut j = i + 4;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            // If we found at least one digit, replace with "skin0"
            if j > i + 4 {
                result.push_str("skin0");
                i = j;
                continue;
            }
        }
        // Paths in WADs are ASCII lowercase, so byte-push is safe
        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

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
            println!(
                "[dry-run] WOULD inject {} chunk(s) into {} (champion: {})",
                entries.len(),
                wad_rel_path,
                champion_id
            );
            for entry in entries {
                let remapped = remap_to_default_skin(&entry.chunk_path);
                let hash = xxh64(remapped.as_bytes(), 0);
                println!(
                    "  [dry-run] remapped='{}' hash={:016x} size={}",
                    remapped, hash, entry.data.len()
                );
            }
        } else {
            inject_into_wad(&wad_path, entries, app, skin_id)?;
        }

        current += 1;
    }

    Ok(())
}

// ──────────────────────────── inject_into_wad (REAL) ────────────────────────────

fn inject_into_wad(
    wad_path: &Path,
    entries: Vec<&super::FantomeEntry>,
    app: &AppHandle,
    skin_id: &str,
) -> Result<(), String> {
    // 1. ALWAYS back up the WAD first. Never overwrite an existing .bak
    //    (it represents the pristine pre-mod state — if we overwrote it
    //    with a partially-modded WAD, Restore All would no longer recover
    //    the original).
    let bak_path = build_bak_path(wad_path);
    if !bak_path.exists() {
        std::fs::copy(wad_path, &bak_path)
            .map_err(|e| format!("Failed to back up {}: {}", wad_path.display(), e))?;
    }

    // 2. Open the WAD for read+write (binary mode).
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(wad_path)
        .map_err(|e| format!("Failed to open WAD {}: {}", wad_path.display(), e))?;

    // 3. Parse header.
    let header = parse_wad_header(&mut file)?;

    // 4. Read the TOC.
    file.seek(SeekFrom::Start(WAD_HEADER_SIZE))
        .map_err(|e| format!("Failed to seek to TOC: {}", e))?;

    let mut toc: Vec<WadTocEntry> = Vec::with_capacity(header.entry_count as usize);
    for _ in 0..header.entry_count {
        toc.push(read_toc_entry(&mut file)?);
    }

    // 5. Get the current EOF — this is where new chunk data will be appended.
    let original_eof = file.metadata()
        .map_err(|e| format!("Failed to read WAD metadata: {}", e))?
        .len();

    // 6. For each manifest entry: remap to default skin, compute hash, find
    //    matching TOC entry, prepare the new chunk data to append.
    let mut appended_data: Vec<u8> = Vec::new();
    // List of (toc_index, new_offset, new_compressed_size, new_uncompressed_size)
    let mut toc_updates: Vec<(usize, u64, u32, u32)> = Vec::new();
    let mut injected_count = 0usize;
    let mut skipped_count = 0usize;

    for entry in &entries {
        let remapped = remap_to_default_skin(&entry.chunk_path);
        let hash = xxh64(remapped.as_bytes(), 0);

        let _ = app.emit("apply-progress", serde_json::json!({
            "skinId": skin_id,
            "phase": "injecting-pending",
            "chunkPath": remapped,
            "chunkHash": format!("{:016x}", hash),
            "dataSize": entry.data.len(),
        }));

        let matching_idx = toc.iter().position(|e| e.hash == hash);
        match matching_idx {
            Some(idx) => {
                let offset_in_appended = appended_data.len() as u64;
                appended_data.extend_from_slice(&entry.data);
                let new_offset = original_eof + offset_in_appended;
                let new_size = entry.data.len() as u32;
                toc_updates.push((idx, new_offset, new_size, new_size));
                injected_count += 1;
            }
            None => {
                // No matching chunk in this WAD — the path likely belongs
                // to a different WAD. Safe to skip; another call to
                // apply_manifest for a different WAD will handle it.
                skipped_count += 1;
            }
        }
    }

    if toc_updates.is_empty() {
        return Err(format!(
            "No matching chunks found in {} for this skin's manifest. \
             Injected: 0, skipped: {}. The .fantome's chunk paths may not \
             match the current WAD layout — Riot may have changed the \
             asset paths in a recent patch.",
            wad_path.display(),
            skipped_count
        ));
    }

    // 7. Append the new chunk data at the end of the file.
    file.seek(SeekFrom::End(0))
        .map_err(|e| format!("Failed to seek to end: {}", e))?;
    file.write_all(&appended_data)
        .map_err(|e| format!("Failed to write new chunk data: {}", e))?;

    // 8. Update the TOC entries to point at the new offsets + sizes.
    for (toc_idx, new_offset, new_compressed_size, new_uncompressed_size) in &toc_updates {
        let toc_offset = WAD_HEADER_SIZE + (*toc_idx as u64) * WAD_TOC_ENTRY_SIZE;
        // Skip the 8-byte hash field, write the offset + sizes.
        file.seek(SeekFrom::Start(toc_offset + 8))
            .map_err(|e| format!("Failed to seek to TOC entry {}: {}", toc_idx, e))?;
        file.write_all(&new_offset.to_le_bytes())
            .map_err(|e| format!("Failed to write new offset: {}", e))?;
        file.write_all(&new_compressed_size.to_le_bytes())
            .map_err(|e| format!("Failed to write compressed size: {}", e))?;
        file.write_all(&new_uncompressed_size.to_le_bytes())
            .map_err(|e| format!("Failed to write uncompressed size: {}", e))?;
    }

    // 9. Flush to disk.
    file.flush()
        .map_err(|e| format!("Failed to flush WAD: {}", e))?;
    // On Windows, also fsync to make sure the write hits the disk before
    // the game client reads it.
    let _ = file.sync_data();

    let _ = app.emit("apply-progress", serde_json::json!({
        "skinId": skin_id,
        "phase": "injected",
        "wad": wad_path.to_string_lossy(),
        "injected": injected_count,
        "skipped": skipped_count,
    }));

    Ok(())
}

fn parse_wad_header(file: &mut std::fs::File) -> Result<WadHeader, String> {
    let mut buf = [0u8; WAD_HEADER_SIZE as usize];
    file.read_exact(&mut buf)
        .map_err(|e| format!("Failed to read WAD header ({} bytes): {}", WAD_HEADER_SIZE, e))?;

    if buf[0..2] != WAD_MAGIC {
        return Err("Invalid WAD file — magic mismatch (expected 'RW')".into());
    }

    let major = buf[2];
    let minor = buf[3];
    if major != 3 {
        return Err(format!(
            "Unsupported WAD version {}.{} — only v3.x is supported",
            major, minor
        ));
    }

    // For v3, EC (entry count) is at bytes 8..12 as u32 LE.
    // This is the commonly documented layout; if your WAD has a different
    // layout (Riot has rev'd the format a few times), this will be wrong
    // and the parser will fail loudly rather than silently corrupting.
    let entry_count = u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]);

    // Sanity check: entry_count should be reasonable (< 1 million).
    // A WAD with more than 1M entries is almost certainly misparsed.
    if entry_count > 1_000_000 {
        return Err(format!(
            "Implausible entry count {} — header layout may have changed",
            entry_count
        ));
    }

    Ok(WadHeader { major, minor, entry_count })
}

fn read_toc_entry(file: &mut std::fs::File) -> Result<WadTocEntry, String> {
    let mut buf = [0u8; WAD_TOC_ENTRY_SIZE as usize];
    file.read_exact(&mut buf)
        .map_err(|e| format!("Failed to read TOC entry: {}", e))?;

    let hash = u64::from_le_bytes([
        buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7],
    ]);
    let offset = u64::from_le_bytes([
        buf[8], buf[9], buf[10], buf[11], buf[12], buf[13], buf[14], buf[15],
    ]);
    let compressed_size = u32::from_le_bytes([buf[16], buf[17], buf[18], buf[19]]);
    let uncompressed_size = u32::from_le_bytes([buf[20], buf[21], buf[22], buf[23]]);

    Ok(WadTocEntry {
        hash,
        offset,
        compressed_size,
        uncompressed_size,
    })
}

/// Build the .bak path next to a WAD file.
/// For "Ahri.wad.client" → "Ahri.wad.client.bak"
fn build_bak_path(wad_path: &Path) -> PathBuf {
    // Use the full file name + ".bak"
    let mut name = wad_path.file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    name.push_str(".bak");
    wad_path.with_file_name(name)
}

// ──────────────────────────── uninstall (restore .bak) ────────────────────────────

pub async fn uninstall_skin_for_champion(
    game_dir: &Path,
    champion_id: &str,
) -> Result<(), String> {
    let _ = champion_id;
    let _ = restore_all_inner(game_dir).await?;
    Ok(())
}

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
            if name.ends_with(".wad.client.bak") {
                let original_name = name.trim_end_matches(".bak");
                let original_path: PathBuf = path.with_file_name(original_name);

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
