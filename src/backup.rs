//! Selective backup of mods and/or save games.
//!
//! Backups are written as timestamped folders under
//! `AppPaths::backups/user/<timestamp>/` with optional `mods/` and `saves/`
//! subtrees, plus a small `manifest.json` describing what was included.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::app_paths::AppPaths;
use crate::game::GameInstall;
use crate::saves::{self, SaveFile};
use crate::store::ModStore;

/// What the user chose to include in a backup run.
#[derive(Debug, Clone, Default)]
pub struct BackupSelection {
    /// When false, no mods are copied even if `mod_ids` is non-empty.
    pub include_mods: bool,
    /// Specific mod ids to back up. Empty + `include_mods` means "all mods".
    pub mod_ids: Vec<String>,
    /// When false, no saves are copied.
    pub include_saves: bool,
    /// Specific save file names. Empty + `include_saves` means "all saves".
    pub save_names: Vec<String>,
}

/// Written next to each backup for later restore / inspection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupManifest {
    pub created_at: u64,
    pub game_id: Option<String>,
    pub include_mods: bool,
    pub mod_ids: Vec<String>,
    pub mod_names: Vec<String>,
    pub include_saves: bool,
    pub save_names: Vec<String>,
    pub total_files: u64,
    pub total_bytes: u64,
}

/// Result of a completed backup.
#[derive(Debug, Clone)]
pub struct BackupResult {
    pub dest: PathBuf,
    pub mods_backed_up: usize,
    pub saves_backed_up: usize,
    pub total_files: u64,
    pub total_bytes: u64,
}

/// Create a selective backup under `paths.backups/user/<timestamp>/`.
pub fn create_backup(
    paths: &AppPaths,
    store: &ModStore,
    game: Option<&GameInstall>,
    selection: &BackupSelection,
) -> Result<BackupResult> {
    if !selection.include_mods && !selection.include_saves {
        bail!("nothing selected — enable mods and/or saves to back up");
    }

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let dest = paths.backups.join("user").join(timestamp.to_string());
    fs::create_dir_all(&dest)
        .with_context(|| format!("creating backup directory {}", dest.display()))?;

    let mut total_files = 0u64;
    let mut total_bytes = 0u64;
    let mut mod_ids_done = Vec::new();
    let mut mod_names_done = Vec::new();
    let mut save_names_done = Vec::new();

    if selection.include_mods {
        let mods_root = dest.join("mods");
        fs::create_dir_all(&mods_root)?;
        let wanted: Vec<_> = if selection.mod_ids.is_empty() {
            store.mods.iter().collect()
        } else {
            store
                .mods
                .iter()
                .filter(|m| selection.mod_ids.iter().any(|id| id == &m.id))
                .collect()
        };
        for m in wanted {
            let target = mods_root.join(&m.id);
            let (files, bytes) = copy_tree(&m.content_dir, &target)?;
            total_files += files;
            total_bytes += bytes;
            // Also stash a tiny name sidecar so restore UI can show names
            // without the live store.
            let meta = serde_json::json!({
                "id": m.id,
                "name": m.name,
                "tags": m.tags,
            });
            fs::write(
                mods_root.join(format!("{}.meta.json", m.id)),
                serde_json::to_string_pretty(&meta)?,
            )?;
            mod_ids_done.push(m.id.clone());
            mod_names_done.push(m.name.clone());
        }
    }

    if selection.include_saves {
        let Some(game) = game else {
            bail!("cannot back up saves without an active game install — detect a game first");
        };
        let all = saves::list_saves(game)?;
        let selected: Vec<&SaveFile> = if selection.save_names.is_empty() {
            all.iter().collect()
        } else {
            all.iter()
                .filter(|s| selection.save_names.iter().any(|n| n == &s.name))
                .collect()
        };
        let saves_root = dest.join("saves");
        fs::create_dir_all(&saves_root)?;
        for s in &selected {
            let target = saves_root.join(&s.name);
            fs::copy(&s.path, &target).with_context(|| {
                format!("backing up save {} -> {}", s.path.display(), target.display())
            })?;
            total_files += 1;
            total_bytes += s.size_bytes;
            save_names_done.push(s.name.clone());
        }
    }

    let manifest = BackupManifest {
        created_at: timestamp,
        game_id: game.map(|g| g.id.clone()),
        include_mods: selection.include_mods,
        mod_ids: mod_ids_done.clone(),
        mod_names: mod_names_done,
        include_saves: selection.include_saves,
        save_names: save_names_done.clone(),
        total_files,
        total_bytes,
    };
    fs::write(
        dest.join("manifest.json"),
        serde_json::to_string_pretty(&manifest).context("serializing backup manifest")?,
    )
    .with_context(|| format!("writing manifest in {}", dest.display()))?;

    Ok(BackupResult {
        dest,
        mods_backed_up: mod_ids_done.len(),
        saves_backed_up: save_names_done.len(),
        total_files,
        total_bytes,
    })
}

/// Also package a finished backup folder as a `.zip` next to it.
pub fn zip_backup_folder(backup_dir: &Path) -> Result<PathBuf> {
    if !backup_dir.is_dir() {
        bail!("backup folder does not exist: {}", backup_dir.display());
    }
    let zip_path = backup_dir.with_extension("zip");
    let zip_file = fs::File::create(&zip_path)
        .with_context(|| format!("creating {}", zip_path.display()))?;
    let mut zip = zip::ZipWriter::new(zip_file);
    let options = zip::write::FileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    for entry in walkdir::WalkDir::new(backup_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
    {
        let rel = entry
            .path()
            .strip_prefix(backup_dir)
            .with_context(|| "strip backup prefix")?;
        let name = rel.to_string_lossy().replace('\\', "/");
        zip.start_file(&name, options)
            .with_context(|| format!("adding {name} to zip"))?;
        let mut f = fs::File::open(entry.path())?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf)?;
        zip.write_all(&buf)?;
    }
    zip.finish()?;
    Ok(zip_path)
}

/// List previous user backups (folders under `backups/user/`).
pub fn list_backups(paths: &AppPaths) -> Result<Vec<(PathBuf, Option<BackupManifest>)>> {
    let root = paths.backups.join("user");
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(&root)?.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let manifest = fs::read_to_string(path.join("manifest.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok());
        out.push((path, manifest));
    }
    out.sort_by(|a, b| b.0.cmp(&a.0));
    Ok(out)
}

fn copy_tree(src: &Path, dest: &Path) -> Result<(u64, u64)> {
    if !src.exists() {
        return Ok((0, 0));
    }
    fs::create_dir_all(dest)?;
    let mut files = 0u64;
    let mut bytes = 0u64;
    for entry in walkdir::WalkDir::new(src)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let rel = entry.path().strip_prefix(src)?;
        let target = dest.join(rel);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&target)?;
        } else if entry.file_type().is_file() {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(entry.path(), &target)?;
            files += 1;
            bytes += entry.metadata().map(|m| m.len()).unwrap_or(0);
        }
    }
    Ok((files, bytes))
}
