//! Lightweight BSA (Bethesda Softworks Archive) listing for conflict awareness.
//!
//! We only need the *list of paths* inside each `.bsa` so the conflict viewer
//! can surface overlaps that loose-file scanning misses. Full extraction is
//! intentionally out of scope — the game loads BSAs itself.
//!
//! Supports Skyrim LE (v104) and SE/AE (v105). Corrupt / non-BSA files are
//! skipped rather than aborting a whole profile scan.

use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

const FLAG_INCLUDE_DIR_NAMES: u32 = 0x1;
const FLAG_INCLUDE_FILE_NAMES: u32 = 0x2;

/// One BSA archive found in a mod, with every internal path it contains.
#[derive(Debug, Clone)]
pub struct BsaArchive {
    /// File name of the `.bsa` (e.g. `MyMod.bsa`).
    pub name: String,
    /// Absolute path to the archive on disk.
    pub path: PathBuf,
    /// Internal paths, normalized to forward slashes, lowercased.
    pub files: Vec<String>,
}

/// Find every `.bsa` at the top level of a mod's content directory and list
/// the files inside each one. Nested BSAs (rare) are ignored — Skyrim only
/// loads archives sitting directly in `Data`.
pub fn scan_mod_bsas(content_dir: &Path) -> Vec<(String, BsaArchive)> {
    let Ok(entries) = std::fs::read_dir(content_dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.to_lowercase().ends_with(".bsa") {
            continue;
        }
        match list_bsa_files(&path) {
            Ok(files) => out.push((
                name.clone(),
                BsaArchive {
                    name,
                    path,
                    files,
                },
            )),
            Err(_) => {
                // Unreadable / exotic BSA — still surface the archive itself
                // so the person knows it exists, just with an empty file list.
                out.push((
                    name.clone(),
                    BsaArchive {
                        name,
                        path,
                        files: Vec::new(),
                    },
                ));
            }
        }
    }
    out
}

/// Given per-mod lists of `(bsa_name, BsaArchive)`, find internal paths that
/// appear in more than one mod's BSA(s). Returns `(internal_path, Vec<(mod_id, bsa_name)>)`.
///
/// No "winner" is asserted: BSA precedence depends on plugin load order and
/// whether the archive is registered via a same-named plugin or INI, which is
/// outside this tool's control. The list is purely informational.
pub fn find_bsa_overlaps(
    mod_bsas: &[(String, Vec<(String, BsaArchive)>)],
) -> Vec<(String, Vec<(String, String)>)> {
    // path_lower -> Vec<(mod_id, bsa_name)>
    let mut index: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for (mod_id, archives) in mod_bsas {
        for (bsa_name, archive) in archives {
            for file in &archive.files {
                index
                    .entry(file.clone())
                    .or_default()
                    .push((mod_id.clone(), bsa_name.clone()));
            }
        }
    }
    let mut overlaps: Vec<(String, Vec<(String, String)>)> = index
        .into_iter()
        .filter(|(_, contributors)| {
            let mut mods: Vec<&str> = contributors.iter().map(|(m, _)| m.as_str()).collect();
            mods.sort();
            mods.dedup();
            mods.len() > 1
        })
        .collect();
    overlaps.sort_by(|a, b| a.0.cmp(&b.0));
    overlaps
}

/// Parse a BSA and return every internal file path as `folder/file`, using
/// forward slashes and lower case (matching how the conflict viewer keys
/// loose files).
pub fn list_bsa_files(path: &Path) -> Result<Vec<String>> {
    let mut file = File::open(path).with_context(|| format!("opening BSA {}", path.display()))?;
    let mut header = [0u8; 36];
    file.read_exact(&mut header)
        .with_context(|| format!("reading BSA header {}", path.display()))?;
    if &header[0..4] != b"BSA\0" {
        bail!("{} is not a BSA (bad magic)", path.display());
    }
    let version = u32::from_le_bytes(header[4..8].try_into().unwrap());
    if version != 104 && version != 105 {
        bail!(
            "{} has unsupported BSA version {version} (need 104 or 105)",
            path.display()
        );
    }
    let archive_flags = u32::from_le_bytes(header[12..16].try_into().unwrap());
    let folder_count = u32::from_le_bytes(header[16..20].try_into().unwrap()) as usize;
    let file_count = u32::from_le_bytes(header[20..24].try_into().unwrap()) as usize;
    let total_file_name_length =
        u32::from_le_bytes(header[28..32].try_into().unwrap()) as usize;
    let has_dir_names = archive_flags & FLAG_INCLUDE_DIR_NAMES != 0;
    let has_file_names = archive_flags & FLAG_INCLUDE_FILE_NAMES != 0;

    // Folder records: v104 = 16 bytes, v105 = 24 bytes.
    let folder_rec_size = if version == 105 { 24 } else { 16 };
    let mut folder_counts = Vec::with_capacity(folder_count);
    for _ in 0..folder_count {
        let mut rec = vec![0u8; folder_rec_size];
        file.read_exact(&mut rec)
            .context("reading BSA folder record")?;
        // count is at offset 8 for both versions.
        let count = u32::from_le_bytes(rec[8..12].try_into().unwrap()) as usize;
        folder_counts.push(count);
    }

    // File-record blocks: optional bzstring folder name + file records (16 bytes each).
    let mut folders: Vec<(String, usize)> = Vec::with_capacity(folder_count);
    for &count in &folder_counts {
        let folder_name = if has_dir_names {
            read_bzstring(&mut file)?.to_lowercase().replace('/', "\\")
        } else {
            String::new()
        };
        // Skip file records — we only need the names block for paths.
        let skip = count.saturating_mul(16);
        file.seek(SeekFrom::Current(skip as i64))
            .context("seeking past BSA file records")?;
        folders.push((folder_name, count));
    }

    if !has_file_names {
        // Without names we can't report useful paths.
        return Ok(Vec::new());
    }

    let mut name_block = vec![0u8; total_file_name_length];
    file.read_exact(&mut name_block)
        .context("reading BSA file name block")?;

    let file_names = split_c_strings(&name_block);
    if file_names.len() < file_count {
        // Truncated name block — use what we have.
    }

    let mut out = Vec::with_capacity(file_count);
    let mut name_idx = 0usize;
    for (folder, count) in folders {
        for _ in 0..count {
            let file_name = file_names
                .get(name_idx)
                .map(|s| s.as_str())
                .unwrap_or("")
                .to_lowercase();
            name_idx += 1;
            if file_name.is_empty() {
                continue;
            }
            let full = if folder.is_empty() || folder == "." {
                file_name
            } else {
                format!("{folder}\\{file_name}")
            };
            // Normalize to forward slashes for display / overlap keys.
            out.push(full.replace('\\', "/"));
        }
    }
    Ok(out)
}

/// bzstring: u8 length (includes trailing NUL), then length bytes of data.
fn read_bzstring(file: &mut File) -> Result<String> {
    let mut len_buf = [0u8; 1];
    file.read_exact(&mut len_buf).context("reading bzstring length")?;
    let len = len_buf[0] as usize;
    if len == 0 {
        return Ok(String::new());
    }
    let mut buf = vec![0u8; len];
    file.read_exact(&mut buf).context("reading bzstring data")?;
    // Drop trailing NUL if present.
    if buf.last() == Some(&0) {
        buf.pop();
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

fn split_c_strings(block: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut start = 0usize;
    for (i, &b) in block.iter().enumerate() {
        if b == 0 {
            if i > start {
                out.push(String::from_utf8_lossy(&block[start..i]).into_owned());
            }
            start = i + 1;
        }
    }
    if start < block.len() {
        out.push(String::from_utf8_lossy(&block[start..]).into_owned());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_bsa() {
        let dir = std::env::temp_dir().join(format!("bsa-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("not.bsa");
        std::fs::write(&path, b"XXXX").unwrap();
        assert!(list_bsa_files(&path).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_mod_has_no_bsas() {
        let dir = std::env::temp_dir().join(format!("bsa-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(scan_mod_bsas(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn overlap_requires_two_mods() {
        let a = BsaArchive {
            name: "a.bsa".into(),
            path: PathBuf::from("a.bsa"),
            files: vec!["meshes/x.nif".into(), "textures/y.dds".into()],
        };
        let b = BsaArchive {
            name: "b.bsa".into(),
            path: PathBuf::from("b.bsa"),
            files: vec!["meshes/x.nif".into()],
        };
        let input = vec![
            ("mod1".into(), vec![("a.bsa".into(), a)]),
            ("mod2".into(), vec![("b.bsa".into(), b)]),
        ];
        let overlaps = find_bsa_overlaps(&input);
        assert_eq!(overlaps.len(), 1);
        assert_eq!(overlaps[0].0, "meshes/x.nif");
        assert_eq!(overlaps[0].1.len(), 2);
    }
}
