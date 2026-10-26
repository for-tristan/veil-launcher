// wad.rs — Native WAD chunk injection (DEAD CODE — Veil switched to ltk_patcher
// subprocess approach in commit 0c551f9. This module is kept for reference
// in case the file-modification path needs to be revived.)
//
// Suppress all dead_code warnings since this module is intentionally unused.

#![allow(dead_code)]
#![allow(unused_variables)]
#![allow(unused_imports)]

// FORMAT (per https://www.communitydragon.org/docs/wad):
//
//   WAD header (4 bytes):
//     bytes 0-1: "RW" magic
//     byte 2:   major version (3)
//     byte 3:   minor version (0-4+)
//
//   Directory header (268 bytes, immediately after WAD header):
//     bytes 0-255   (file bytes 4-259):   256-byte ECDSA signature
//     bytes 256-263 (file bytes 260-267): 8-byte header checksum
//                                         (xxhash64 of entry headers
//                                          sorted by data offset)
//     bytes 264-267 (file bytes 268-271): u32 LE entry count (EC)
//
//   Total header size = 4 + 268 = 272 bytes.
//
//   TOC entries (32 bytes each, immediately after header at byte 272):
//     bytes 0-7:   u64 LE path hash (xxhash64 of chunk path with seed=0)
//     bytes 8-11:  u32 LE data offset (offset from start of file)
//     bytes 12-15: u32 LE compressed size (size on disk)
//     bytes 16-19: u32 LE uncompressed size (size in memory)
//     byte 20:     high 4 bits = data type (0=raw, 1=gzip, 2=redirect,
//                  3=zstd, 4=zstd-subchunks), low 4 bits = subchunk count
//     byte 21:     for v3.4+: high byte of first_subchunk_idx
//                  (was: bool duplicate flag in older versions)
//     bytes 22-23: u16 LE low 2 bytes of first_subchunk_idx
//     bytes 24-31: 8-byte entry checksum (xxhash64 of chunk data)
//
// INJECTION STRATEGY:
//   1. Backup the WAD to <wad>.wad.client.bak (always).
//   2. Read the 272-byte header + TOC entries.
//   3. For each .fantome manifest entry:
//        - Remap the chunk path: replace "skin<digits>" with "skin0"
//          so the modded asset OVERRIDES the champion's default skin slot
//          (this is what makes the skin show when picking Classic in
//          champ select).
//        - Compute xxhash64 of the remapped path (seed=0).
//        - Find the matching TOC entry by hash.
//        - Append the new chunk data at the end of the file.
//        - Update the TOC entry: offset = new EOF position,
//          comp_size = uncomp_size = data.len(), type = 0 (uncompressed),
//          subchunks = 0, first_subchunk_idx = 0.
//   4. Compute the new header checksum (xxhash64 of all entries sorted
//      by offset) and write it to bytes 260-267. This is required for
//      the LoL client to accept the modified WAD.
//   5. Compute the new entry checksum for each modified entry (xxhash64
//      of the chunk data) and write it to bytes 24-31 of the entry.
//   6. Flush + sync_data to hit disk.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter};
use xxhash_rust::xxh64::xxh64;

const WAD_MAGIC: [u8; 2] = *b"RW";
const WAD_HEADER_SIZE: u64 = 272;
const WAD_TOC_ENTRY_SIZE: usize = 32;
const HEADER_CHECKSUM_OFFSET: u64 = 260;
const EC_OFFSET: u64 = 268;
const ENTRY_CHECKSUM_OFFSET: usize = 24;

// ──────────────────────────── Default skin remapping ────────────────────────────

/// Remap a chunk path so it targets the champion's default skin slot
/// (skin0) instead of the original skinN slot. This is what makes Veil's
/// applied skins OVERRIDE the DEFAULT skin shown in champion select.
fn remap_to_default_skin(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut result = String::with_capacity(path.len() + 8);
    let mut i = 0usize;
    while i < bytes.len() {
        if i + 4 <= bytes.len() && &bytes[i..i + 4] == b"skin" {
            let mut j = i + 4;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j > i + 4 {
                result.push_str("skin0");
                i = j;
                continue;
            }
        }
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

// ──────────────────────────── inject_into_wad (REAL, correct format) ────────────────────────────

fn inject_into_wad(
    wad_path: &Path,
    entries: Vec<&super::FantomeEntry>,
    app: &AppHandle,
    skin_id: &str,
) -> Result<(), String> {
    // 1. ALWAYS back up the WAD first.
    let bak_path = build_bak_path(wad_path);
    if !bak_path.exists() {
        std::fs::copy(wad_path, &bak_path)
            .map_err(|e| format!("Failed to back up {}: {}", wad_path.display(), e))?;
    }

    // 2. Open the WAD for read+write.
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(wad_path)
        .map_err(|e| format!("Failed to open WAD {}: {}", wad_path.display(), e))?;

    // 3. Parse the 272-byte header.
    let header = parse_wad_header(&mut file)?;

    // 4. Read the TOC entries (32 bytes each, starting at byte 272).
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

    // 6. For each manifest entry: remap path, compute hash, find matching
    //    TOC entry, prepare new chunk data + TOC entry update.
    let mut appended_data: Vec<u8> = Vec::new();
    let mut toc_updates: Vec<(usize, Vec<u8>)> = Vec::new();
    // (toc_index, new 32-byte entry bytes)
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

        let matching_idx = toc.iter().position(|e| e.path_hash == hash);
        match matching_idx {
            Some(idx) => {
                let offset_in_appended = appended_data.len() as u64;
                appended_data.extend_from_slice(&entry.data);
                let new_offset = original_eof + offset_in_appended;
                let new_size = entry.data.len() as u32;

                // Compute the entry checksum (xxhash64 of chunk data, seed=0).
                let entry_checksum = xxh64(&entry.data, 0);

                // Build the new 32-byte TOC entry.
                let mut new_entry_bytes = [0u8; WAD_TOC_ENTRY_SIZE];
                new_entry_bytes[0..8].copy_from_slice(&hash.to_le_bytes());
                new_entry_bytes[8..12].copy_from_slice(&new_offset.to_le_bytes());
                new_entry_bytes[12..16].copy_from_slice(&new_size.to_le_bytes());        // comp_size
                new_entry_bytes[16..20].copy_from_slice(&new_size.to_le_bytes());        // uncomp_size
                new_entry_bytes[20] = 0;                                                  // type=0 (uncompressed) | subchunks=0
                new_entry_bytes[21] = 0;                                                  // high byte of first_subchunk_idx (v3.4+)
                new_entry_bytes[22..24].copy_from_slice(&0u16.to_le_bytes());            // low 2 bytes of first_subchunk_idx
                new_entry_bytes[24..32].copy_from_slice(&entry_checksum.to_le_bytes()); // entry checksum

                toc_updates.push((idx, new_entry_bytes.to_vec()));
                injected_count += 1;
            }
            None => {
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

    // 7. Append the new chunk data at EOF.
    file.seek(SeekFrom::End(0))
        .map_err(|e| format!("Failed to seek to end: {}", e))?;
    file.write_all(&appended_data)
        .map_err(|e| format!("Failed to write new chunk data: {}", e))?;

    // 8. Write the updated TOC entries back to the file (each at its
    //    original TOC offset, NOT at the new chunk data location).
    for (toc_idx, new_entry_bytes) in &toc_updates {
        let toc_offset = WAD_HEADER_SIZE + (*toc_idx as u64) * (WAD_TOC_ENTRY_SIZE as u64);
        file.seek(SeekFrom::Start(toc_offset))
            .map_err(|e| format!("Failed to seek to TOC entry {}: {}", toc_idx, e))?;
        file.write_all(new_entry_bytes)
            .map_err(|e| format!("Failed to write TOC entry {}: {}", toc_idx, e))?;
    }

    // 9. Recompute the header checksum (xxhash64 of all entries sorted by
    //    data offset) and write it to bytes 260-267.
    let mut updated_toc = toc.clone();
    for (toc_idx, new_entry_bytes) in &toc_updates {
        if let Some(existing) = updated_toc.get_mut(*toc_idx) {
            // Update the in-memory TOC entry to reflect the new state so
            // the checksum computation matches what's now on disk.
            existing.data_offset = u32::from_le_bytes(
                new_entry_bytes[8..12].try_into().unwrap()
            );
            existing.compressed_size = u32::from_le_bytes(
                new_entry_bytes[12..16].try_into().unwrap()
            );
            existing.uncompressed_size = u32::from_le_bytes(
                new_entry_bytes[16..20].try_into().unwrap()
            );
        }
    }
    let mut sorted_for_checksum = updated_toc.clone();
    sorted_for_checksum.sort_by_key(|e| e.data_offset);

    // Build the concatenated bytes of all entries sorted by offset.
    let mut hasher_bytes: Vec<u8> = Vec::with_capacity(toc.len() * WAD_TOC_ENTRY_SIZE);
    for entry in &sorted_for_checksum {
        // Read the entry's current bytes from the file (since some fields
        // like the path_hash and checksum aren't in our in-memory struct).
        let toc_offset = WAD_HEADER_SIZE
            + (toc.iter().position(|e| std::ptr::eq(e, entry)).unwrap_or(0) as u64)
            * (WAD_TOC_ENTRY_SIZE as u64);
        // Actually simpler: just read each entry fresh from disk in
        // sorted-by-offset order.
        let _ = toc_offset; // unused in this version
    }
    // Re-read each entry's 32 bytes from disk in sorted-by-offset order.
    let mut indices: Vec<usize> = (0..toc.len()).collect();
    indices.sort_by_key(|&i| updated_toc[i].data_offset);
    for idx in &indices {
        let toc_offset = WAD_HEADER_SIZE + (*idx as u64) * (WAD_TOC_ENTRY_SIZE as u64);
        file.seek(SeekFrom::Start(toc_offset))
            .map_err(|e| format!("Failed to seek to TOC entry {}: {}", idx, e))?;
        let mut entry_buf = [0u8; WAD_TOC_ENTRY_SIZE];
        file.read_exact(&mut entry_buf)
            .map_err(|e| format!("Failed to read TOC entry {}: {}", idx, e))?;
        hasher_bytes.extend_from_slice(&entry_buf);
    }
    let header_checksum = xxh64(&hasher_bytes, 0);
    file.seek(SeekFrom::Start(HEADER_CHECKSUM_OFFSET))
        .map_err(|e| format!("Failed to seek to header checksum: {}", e))?;
    file.write_all(&header_checksum.to_le_bytes())
        .map_err(|e| format!("Failed to write header checksum: {}", e))?;

    // 10. Flush + sync_data.
    file.flush().map_err(|e| format!("Failed to flush WAD: {}", e))?;
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

struct WadHeader {
    #[allow(dead_code)]
    major: u8,
    #[allow(dead_code)]
    minor: u8,
    entry_count: u32,
}

#[derive(Clone)]
struct WadTocEntry {
    path_hash: u64,
    data_offset: u32,
    #[allow(dead_code)]
    compressed_size: u32,
    #[allow(dead_code)]
    uncompressed_size: u32,
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

    // EC at file bytes 268-271 as u32 LE (CommunityDragon spec).
    let entry_count = u32::from_le_bytes([buf[268], buf[269], buf[270], buf[271]]);

    // Sanity check.
    if entry_count > 5_000_000 {
        return Err(format!(
            "Implausible entry count {} — header layout may have changed",
            entry_count
        ));
    }

    Ok(WadHeader { major, minor, entry_count })
}

fn read_toc_entry(file: &mut std::fs::File) -> Result<WadTocEntry, String> {
    let mut buf = [0u8; WAD_TOC_ENTRY_SIZE];
    file.read_exact(&mut buf)
        .map_err(|e| format!("Failed to read TOC entry: {}", e))?;

    let path_hash = u64::from_le_bytes(buf[0..8].try_into().unwrap());
    let data_offset = u32::from_le_bytes(buf[8..12].try_into().unwrap());
    let compressed_size = u32::from_le_bytes(buf[12..16].try_into().unwrap());
    let uncompressed_size = u32::from_le_bytes(buf[16..20].try_into().unwrap());

    Ok(WadTocEntry {
        path_hash,
        data_offset,
        compressed_size,
        uncompressed_size,
    })
}

fn build_bak_path(wad_path: &Path) -> PathBuf {
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
