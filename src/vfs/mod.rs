//! Virtual file system deployment.
//!
//! The algorithm is platform-independent: given a profile's enabled mods (in
//! priority order) and the mod store, build a "winning file" map (last mod
//! in load order wins per relative path), then hand that map to a
//! `LinkBackend` which actually creates the links on disk. Only the link
//! primitive differs between Linux (symlinks) and Windows (junctions +
//! hardlinks).
//!
//! Deploy targets a staging directory, which is then linked over the game's
//! real `Data` folder (after backing up the original `Data` folder once).
//! This means `Data` itself is *never* mutated directly — disabling a
//! profile / mod manager restores the untouched vanilla folder.

use anyhow::Result;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::app_paths::AppPaths;
use crate::game::GameInstall;
use crate::profile::Profile;
use crate::store::ModStore;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::LinuxBackend as PlatformBackend;

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::WindowsBackend as PlatformBackend;

/// Abstraction over "link this store file into the staging tree" so the
/// deploy algorithm below doesn't care whether that's a symlink (Linux) or
/// a junction/hardlink (Windows).
pub trait LinkBackend {
    /// Link a single file from `source` (inside the mod store) to `dest`
    /// (inside the staging tree). Implementations should create parent
    /// directories as needed and replace any existing entry at `dest`.
    fn link_file(&self, source: &Path, dest: &Path) -> Result<()>;

    /// Make `staging_dir` appear at `data_dir` (the game's real Data
    /// folder). Implementations back up any pre-existing real `Data` folder
    /// into `backup_dir` the first time this runs.
    fn mount_staging_over_data(
        &self,
        staging_dir: &Path,
        data_dir: &Path,
        backup_dir: &Path,
    ) -> Result<()>;

    /// Undo `mount_staging_over_data`, restoring the original `Data` folder
    /// from `backup_dir`.
    fn unmount(&self, data_dir: &Path, backup_dir: &Path) -> Result<()>;
}

/// Build the "winning file per relative path" map for a profile: walk each
/// enabled mod's content dir in load order, recording every file's relative
/// path. Later mods overwrite earlier ones on conflict, matching classic
/// MO2/Vortex semantics.
///
/// Paths are matched **case-insensitively** when deciding what counts as
/// "the same file", because that's how the game actually sees them: NTFS
/// (native Windows) and Wine's filesystem emulation both do case-insensitive
/// lookups, so `Textures/Armor.dds` and `textures/armor.dds` are the same
/// file to Skyrim even though they'd be two different files on a
/// case-sensitive Linux filesystem. Without this, two mods using different
/// casing for the same path would both "win" (each landing as a separate
/// symlink) instead of one correctly overriding the other — a real
/// correctness bug that only shows up on Linux, since a Windows install
/// would silently coalesce them at the filesystem level. The winning file's
/// own casing is preserved for the actual on-disk destination path.
pub fn resolve_conflicts(
    store: &ModStore,
    profile: &Profile,
) -> Result<BTreeMap<PathBuf, PathBuf>> {
    let detailed = resolve_conflicts_detailed(store, profile)?;
    Ok(detailed
        .into_iter()
        .map(|(_, contributions)| {
            let winner = contributions.last().expect("at least one contributor");
            (winner.relative_path.clone(), winner.source_path.clone())
        })
        .collect())
}

/// One mod's contribution to a given (case-insensitively matched) relative
/// path.
#[derive(Debug, Clone)]
pub struct Contribution {
    pub mod_id: String,
    pub relative_path: PathBuf,
    pub source_path: PathBuf,
}

/// Like `resolve_conflicts`, but keeps every contributing mod per path (in
/// load order, last = winner) instead of only the winner. Used for the
/// conflict report / "which mod provides this file" tooling.
pub fn resolve_conflicts_detailed(
    store: &ModStore,
    profile: &Profile,
) -> Result<BTreeMap<String, Vec<Contribution>>> {
    let mut by_lower_path: BTreeMap<String, Vec<Contribution>> = BTreeMap::new();

    for mod_id in profile.enabled_mods_in_order() {
        let Some(entry) = store.get(mod_id) else {
            continue; // stale reference to a removed mod; skip quietly
        };
        for file in walkdir::WalkDir::new(&entry.content_dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
        {
            let rel = file
                .path()
                .strip_prefix(&entry.content_dir)
                .expect("walked path is under content_dir")
                .to_path_buf();
            let key = rel.to_string_lossy().to_lowercase();
            by_lower_path.entry(key).or_default().push(Contribution {
                mod_id: mod_id.to_string(),
                relative_path: rel,
                source_path: file.path().to_path_buf(),
            });
        }
    }

    Ok(by_lower_path)
}

/// Same computation as `deploy` but touches nothing on disk — useful to
/// preview file counts and conflicts before committing.
pub fn deploy_dry_run(store: &ModStore, profile: &Profile) -> Result<DryRunReport> {
    let detailed = resolve_conflicts_detailed(store, profile)?;
    let mut conflicts = Vec::new();
    let mut total_files = 0usize;
    for (_, contributions) in &detailed {
        total_files += 1;
        if contributions.len() > 1 {
            conflicts.push(contributions.clone());
        }
    }
    Ok(DryRunReport {
        total_files,
        conflicts,
    })
}

#[derive(Debug)]
pub struct DryRunReport {
    pub total_files: usize,
    /// Each entry is every mod that touches a given path, in load order
    /// (last one is the one that will actually win).
    pub conflicts: Vec<Vec<Contribution>>,
}

/// Undo a deploy for a given game, restoring its original (vanilla) `Data`
/// folder from backup. Safe to call even if nothing was ever deployed (it's
/// a no-op in that case).
pub fn restore(paths: &AppPaths, backend: &impl LinkBackend, game: &GameInstall) -> Result<()> {
    let backup_dir = paths.backups.join(&game.id);
    backend.unmount(&game.data_dir, &backup_dir)
}

/// Full deploy: resolve conflicts, stage the links, then mount the staging
/// tree over the game's Data folder. Returns the number of files linked.
pub fn deploy(
    paths: &AppPaths,
    backend: &impl LinkBackend,
    store: &ModStore,
    profile: &Profile,
    game: &GameInstall,
) -> Result<usize> {
    let staging_dir = paths.root.join("staging").join(&profile.name);
    if staging_dir.exists() {
        std::fs::remove_dir_all(&staging_dir)?;
    }
    std::fs::create_dir_all(&staging_dir)?;

    let winners = resolve_conflicts(store, profile)?;
    for (rel, source) in &winners {
        let dest = staging_dir.join(rel);
        backend.link_file(source, &dest)?;
    }

    let backup_dir = paths.backups.join(&game.id);
    backend.mount_staging_over_data(&staging_dir, &game.data_dir, &backup_dir)?;

    write_plugins_txt(&game.plugins_txt, &profile.plugin_order)?;

    // Safety net: snapshot current saves before this deploy, in case a new
    // mod combination corrupts a save or the game refuses to load one.
    // Failure here should never block the deploy itself — it's a nice-to-
    // have, not a requirement — so errors are swallowed.
    let _ = backup_saves(paths, game);

    Ok(winners.len())
}

/// Copy the game's `Saves` folder into
/// `backups/<game-id>/saves/<unix-timestamp>/` so a bad mod combination
/// never costs someone their save file. Keeps every snapshot (cheap: saves
/// are typically a few MB each) — pruning old ones is a manual "next
/// version" feature, not something to do silently with someone's saves.
fn backup_saves(paths: &AppPaths, game: &GameInstall) -> Result<()> {
    let Some(my_games_dir) = game.plugins_txt.parent() else {
        return Ok(());
    };
    let saves_dir = my_games_dir.join("Saves");
    if !saves_dir.is_dir() {
        return Ok(());
    }
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let dest = paths
        .backups
        .join(&game.id)
        .join("saves")
        .join(timestamp.to_string());
    std::fs::create_dir_all(&dest)?;
    for entry in walkdir::WalkDir::new(&saves_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
    {
        let rel = entry.path().strip_prefix(&saves_dir)?;
        let target = dest.join(rel);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(entry.path(), &target)?;
    }
    Ok(())
}

fn write_plugins_txt(path: &Path, plugins: &[String]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut contents = String::new();
    for plugin in plugins {
        contents.push('*'); // '*' marks a plugin enabled, matching Skyrim SE's format
        contents.push_str(plugin);
        contents.push('\n');
    }
    std::fs::write(path, contents)?;
    Ok(())
}
