use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::app_paths::AppPaths;
use crate::config::LoadOutcome;

/// Metadata for one mod sitting in the global store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModEntry {
    pub id: String,
    pub name: String,
    pub version: Option<String>,
    /// Seconds since the Unix epoch.
    pub installed_at: u64,
    /// Path (inside `AppPaths::mods`) that contains the mod's Data-relative
    /// file tree, e.g. `<store>/<id>/` containing `meshes/`, `textures/`,
    /// `SomePlugin.esp`, etc. This is the directory the VFS layer will
    /// mirror via symlinks/junctions.
    pub content_dir: PathBuf,
    /// True once we've confirmed the archive/folder root actually contains
    /// game-relevant files at the top level (as opposed to being nested one
    /// directory too deep, which is extremely common with Nexus zips).
    pub root_normalized: bool,
    /// Free-form labels the person can attach for organizing/filtering the
    /// store (e.g. "textures", "armor", "overhaul", "wip").
    #[serde(default)]
    pub tags: Vec<String>,
}

/// The global, profile-independent list of installed mods.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ModStore {
    pub mods: Vec<ModEntry>,
}

const MANIFEST_FILE: &str = "store.json";

impl ModStore {
    pub fn load(paths: &AppPaths) -> Result<Self> {
        match Self::load_with_repair(paths)? {
            LoadOutcome::Ok(s) | LoadOutcome::Missing(s) => Ok(s),
            LoadOutcome::Repaired { value, backup_path } => {
                eprintln!(
                    "warning: store.json was corrupt and has been reset to an empty store. \
                     Broken file kept at {}. Installed mod folders under mods/ were NOT deleted \
                     — you may need to re-register them.",
                    backup_path.display()
                );
                Ok(value)
            }
        }
    }

    /// Load the mod store, quarantining a corrupt `store.json` instead of
    /// crashing. Does not delete mod content directories.
    pub fn load_with_repair(paths: &AppPaths) -> Result<LoadOutcome<Self>> {
        let manifest = paths.root.join(MANIFEST_FILE);
        if !manifest.exists() {
            return Ok(LoadOutcome::Missing(Self::default()));
        }
        let data = fs::read_to_string(&manifest)
            .with_context(|| format!("reading mod store {}", manifest.display()))?;
        match serde_json::from_str::<Self>(&data) {
            Ok(s) => Ok(LoadOutcome::Ok(s)),
            Err(e) => {
                let backup = paths.root.join(format!(
                    "store.json.corrupt.{}",
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0)
                ));
                fs::rename(&manifest, &backup).with_context(|| {
                    format!(
                        "quarantining corrupt store {} -> {} (parse error: {e})",
                        manifest.display(),
                        backup.display()
                    )
                })?;
                let value = Self::default();
                value.save(paths)?;
                Ok(LoadOutcome::Repaired {
                    value,
                    backup_path: backup,
                })
            }
        }
    }

    pub fn save(&self, paths: &AppPaths) -> Result<()> {
        let manifest = paths.root.join(MANIFEST_FILE);
        let data = serde_json::to_string_pretty(self).context("serializing store.json")?;
        fs::write(&manifest, data)
            .with_context(|| format!("writing mod store {}", manifest.display()))?;
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<&ModEntry> {
        self.mods.iter().find(|m| m.id == id)
    }

    /// Remove a mod from the store, deleting its files on disk and also
    /// scrubbing it out of every profile's load order so profiles never end
    /// up silently referencing a mod that no longer exists.
    pub fn remove(&mut self, paths: &AppPaths, id: &str) -> Result<()> {
        if let Some(pos) = self.mods.iter().position(|m| m.id == id) {
            let entry = self.mods.remove(pos);
            if entry.content_dir.exists() {
                fs::remove_dir_all(&entry.content_dir)?;
            }
            self.save(paths)?;

            for name in crate::profile::Profile::list_all(paths).unwrap_or_default() {
                if let Ok(mut profile) = crate::profile::Profile::load(paths, &name) {
                    let before = profile.mod_order.len();
                    profile.mod_order.retain(|m| m.mod_id != id);
                    if profile.mod_order.len() != before {
                        let _ = profile.save(paths);
                    }
                }
            }
        }
        Ok(())
    }

    /// Add a tag to a mod (no-op if already present).
    pub fn add_tag(&mut self, paths: &AppPaths, id: &str, tag: &str) -> Result<()> {
        if let Some(m) = self.mods.iter_mut().find(|m| m.id == id) {
            if !m.tags.iter().any(|t| t == tag) {
                m.tags.push(tag.to_string());
                self.save(paths)?;
            }
        }
        Ok(())
    }

    /// Every distinct tag currently in use across the store, alphabetically.
    /// Pure in-memory query — no fs I/O, so no `.with_context()` needed here
    /// (that convention is for fallible filesystem operations specifically).
    pub fn all_tags(&self) -> Vec<String> {
        let mut tags: Vec<String> = self
            .mods
            .iter()
            .flat_map(|m| m.tags.iter().cloned())
            .collect();
        tags.sort();
        tags.dedup();
        tags
    }

    /// Every mod carrying a given tag.
    pub fn mods_with_tag<'a>(&'a self, tag: &'a str) -> impl Iterator<Item = &'a ModEntry> {
        self.mods.iter().filter(move |m| m.tags.iter().any(|t| t == tag))
    }

    /// Install a mod from a source path, which may be:
    ///  - a directory (already-extracted mod)
    ///  - a `.zip` archive
    ///  - a `.7z` archive
    ///
    /// `display_name` is what shows up in the UI; if not given we derive it
    /// from the file/folder name.
    pub fn install(
        &mut self,
        paths: &AppPaths,
        source: &Path,
        display_name: Option<String>,
    ) -> Result<String> {
        self.install_with_progress(paths, source, display_name, None)
    }

    /// Like `install`, but reports stage messages through `progress` (e.g. for
    /// a CLI progress line on large archives).
    pub fn install_with_progress(
        &mut self,
        paths: &AppPaths,
        source: &Path,
        display_name: Option<String>,
        progress: Option<&dyn Fn(&str)>,
    ) -> Result<String> {
        if !source.exists() {
            bail!(
                "source path does not exist: {}\n\
                 Hint: check the path for typos, or pass a folder/.zip/.7z/.tar.gz/loose file.",
                source.display()
            );
        }

        let id = Uuid::new_v4().to_string();
        let content_dir = paths.mods.join(&id);
        fs::create_dir_all(&content_dir).with_context(|| {
            format!("creating mod content directory {}", content_dir.display())
        })?;

        let report = |msg: &str| {
            if let Some(cb) = progress {
                cb(msg);
            }
        };

        report(&format!("extracting {}…", source.display()));
        if let Err(e) = install_source_into(source, &content_dir) {
            // Don't leave a half-extracted mod dir sitting in the store.
            let _ = fs::remove_dir_all(&content_dir);
            return Err(e).with_context(|| {
                format!("installing from {}", source.display())
            });
        }
        report("normalizing folder layout…");
        normalize_root(&content_dir)
            .with_context(|| format!("normalizing mod root {}", content_dir.display()))?;

        let name = display_name.unwrap_or_else(|| {
            source
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| id.clone())
        });

        let entry = ModEntry {
            id: id.clone(),
            name,
            version: None,
            installed_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            content_dir,
            root_normalized: true,
            tags: Vec::new(),
        };
        self.mods.push(entry);
        self.save(paths)?;
        report("done");
        Ok(id)
    }

    /// Estimate how many bytes (and files) extracting/copying `source` would
    /// consume, without writing anything. Used for install-size preview and
    /// "disk space tight" warnings.
    pub fn estimate_install_size(source: &Path) -> Result<InstallSizeEstimate> {
        if !source.exists() {
            bail!("source path does not exist: {}", source.display());
        }
        if source.is_dir() {
            let mut bytes = 0u64;
            let mut files = 0u64;
            for entry in walkdir::WalkDir::new(source)
                .into_iter()
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().is_file())
            {
                files += 1;
                bytes += entry.metadata().map(|m| m.len()).unwrap_or(0);
            }
            return Ok(InstallSizeEstimate {
                bytes,
                files,
                source_kind: "folder".into(),
            });
        }

        let lower_name = source.to_string_lossy().to_lowercase();
        let ext = source
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase());

        if lower_name.ends_with(".tar.gz") || lower_name.ends_with(".tgz") || ext.as_deref() == Some("tar")
        {
            // Full unpack would be needed for an exact number; use compressed
            // size as a lower bound and note uncertainty.
            let compressed = fs::metadata(source)
                .with_context(|| format!("stat {}", source.display()))?
                .len();
            return Ok(InstallSizeEstimate {
                bytes: compressed.saturating_mul(3), // rough inflate guess
                files: 0,
                source_kind: format!(
                    "tar archive (compressed {} bytes; extracted size estimated ×3)",
                    compressed
                ),
            });
        }

        match ext.as_deref() {
            Some("zip") => {
                let file = fs::File::open(source)
                    .with_context(|| format!("opening zip {}", source.display()))?;
                let mut zip = zip::ZipArchive::new(file)
                    .with_context(|| format!("reading zip central directory {}", source.display()))?;
                let mut bytes = 0u64;
                let mut files = 0u64;
                for i in 0..zip.len() {
                    if let Ok(entry) = zip.by_index(i) {
                        if entry.is_file() {
                            bytes += entry.size();
                            files += 1;
                        }
                    }
                }
                Ok(InstallSizeEstimate {
                    bytes,
                    files,
                    source_kind: "zip".into(),
                })
            }
            Some("7z") => {
                let compressed = fs::metadata(source)?.len();
                Ok(InstallSizeEstimate {
                    bytes: compressed.saturating_mul(4),
                    files: 0,
                    source_kind: format!(
                        "7z archive (compressed {compressed} bytes; extracted size estimated ×4)"
                    ),
                })
            }
            Some("rar") => bail!(
                ".rar is not supported. Re-pack as .zip/.7z or extract to a folder first."
            ),
            _ => {
                let bytes = fs::metadata(source)
                    .with_context(|| format!("stat {}", source.display()))?
                    .len();
                Ok(InstallSizeEstimate {
                    bytes,
                    files: 1,
                    source_kind: "loose file".into(),
                })
            }
        }
    }

    /// Find every enabled (or all) mod that provides a given relative path
    /// (case-insensitive). Returns (mod_id, mod_name, actual relative path).
    pub fn mods_providing_file(&self, relative: &str) -> Vec<(String, String, PathBuf)> {
        let needle = relative.replace('\\', "/").to_lowercase();
        let mut hits = Vec::new();
        for m in &self.mods {
            for file in walkdir::WalkDir::new(&m.content_dir)
                .into_iter()
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().is_file())
            {
                if let Ok(rel) = file.path().strip_prefix(&m.content_dir) {
                    let key = rel.to_string_lossy().replace('\\', "/").to_lowercase();
                    if key == needle || key.ends_with(&needle) || key.contains(&needle) {
                        hits.push((m.id.clone(), m.name.clone(), rel.to_path_buf()));
                    }
                }
            }
        }
        hits
    }

    /// Every esp/esm/esl file name a mod provides (used to auto-populate a
    /// profile's plugin load order when installing straight into a
    /// profile). Returns bare file names (e.g. `MyMod.esp`), since that's
    /// what belongs in `plugins.txt` — Skyrim only loads plugins sitting
    /// directly at the root of `Data`.
    pub fn discover_plugins(&self, id: &str) -> Vec<String> {
        let Some(entry) = self.get(id) else {
            return Vec::new();
        };
        let mut plugins = Vec::new();
        if let Ok(read_dir) = fs::read_dir(&entry.content_dir) {
            for e in read_dir.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                let lower = name.to_lowercase();
                if lower.ends_with(".esp") || lower.ends_with(".esm") || lower.ends_with(".esl") {
                    plugins.push(name);
                }
            }
        }
        plugins
    }

    /// Replace a mod's files in place from a new source (folder/archive),
    /// keeping the same id — so every profile that already references this
    /// mod keeps working after the update instead of needing to be edited.
    pub fn update(&mut self, paths: &AppPaths, id: &str, source: &Path) -> Result<()> {
        let content_dir = {
            let entry = self
                .get(id)
                .with_context(|| format!("no such mod id '{id}' — try list-mods"))?;
            entry.content_dir.clone()
        };
        if !source.exists() {
            bail!("update source does not exist: {}", source.display());
        }
        if content_dir.exists() {
            fs::remove_dir_all(&content_dir).with_context(|| {
                format!("removing old content for mod {id} at {}", content_dir.display())
            })?;
        }
        fs::create_dir_all(&content_dir)
            .with_context(|| format!("recreating content dir {}", content_dir.display()))?;
        install_source_into(source, &content_dir)
            .with_context(|| format!("updating mod {id} from {}", source.display()))?;
        normalize_root(&content_dir)?;
        if let Some(entry) = self.mods.iter_mut().find(|m| m.id == id) {
            entry.installed_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
        }
        self.save(paths)?;
        Ok(())
    }

    /// Allocate a fresh mod id + empty content directory under the store.
    /// Used by multi-step installers (e.g. FOMOD) that need to write files
    /// before the entry is registered in `store.json`.
    pub fn begin_content_dir(paths: &AppPaths) -> Result<(String, PathBuf)> {
        let id = Uuid::new_v4().to_string();
        let content_dir = paths.mods.join(&id);
        fs::create_dir_all(&content_dir).with_context(|| {
            format!("creating mod content directory {}", content_dir.display())
        })?;
        Ok((id, content_dir))
    }

    /// Register a content directory that was already populated (e.g. by a
    /// FOMOD installer) as a store entry. Normalizes the root layout first.
    pub fn register_installed(
        &mut self,
        paths: &AppPaths,
        id: String,
        content_dir: PathBuf,
        display_name: String,
    ) -> Result<()> {
        if !content_dir.is_dir() {
            bail!(
                "content directory does not exist: {}",
                content_dir.display()
            );
        }
        normalize_root(&content_dir)
            .with_context(|| format!("normalizing mod root {}", content_dir.display()))?;
        let entry = ModEntry {
            id: id.clone(),
            name: display_name,
            version: None,
            installed_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            content_dir,
            root_normalized: true,
            tags: Vec::new(),
        };
        self.mods.push(entry);
        self.save(paths)?;
        Ok(())
    }

    /// Disk space used by each mod's content dir, in bytes, plus the total.
    /// Useful for people who install a hundred 4K texture mods and then
    /// wonder where their SSD went.
    pub fn disk_usage(&self) -> (Vec<(String, String, u64)>, u64) {
        let mut per_mod = Vec::new();
        let mut total = 0u64;
        for m in &self.mods {
            let size: u64 = walkdir::WalkDir::new(&m.content_dir)
                .into_iter()
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().is_file())
                .filter_map(|e| e.metadata().ok())
                .map(|meta| meta.len())
                .sum();
            total += size;
            per_mod.push((m.id.clone(), m.name.clone(), size));
        }
        per_mod.sort_by(|a, b| b.2.cmp(&a.2));
        (per_mod, total)
    }
}

/// Many mod archives are packaged as `ModName/Data/meshes/...` or with an
/// extra top-level folder before the actual Data-tree content. This walks
/// down through single-child directories that look like wrappers (a `Data`
/// folder, or a single subfolder with no game files at this level) so the
/// stored content_dir's *top level* is what should be mirrored directly into
/// the game's Data folder.
/// Unwrap common archive wrapper folders so `content_dir`'s top level mirrors
/// what should land under the game's `Data` folder. Public for FOMOD installs.
pub fn normalize_root_public(dir: &Path) -> Result<()> {
    normalize_root(dir)
}

fn normalize_root(dir: &Path) -> Result<()> {
    const GAME_MARKERS: &[&str] = &[
        "meshes", "textures", "scripts", "interface", "sound", "music", "seq", "skse",
    ];

    loop {
        let entries: Vec<_> = fs::read_dir(dir)?.filter_map(|e| e.ok()).collect();

        // If any known Data-tree folder, or an .esp/.esm/.esl file, already
        // exists at this level, we're done normalizing.
        let has_marker = entries.iter().any(|e| {
            let name = e.file_name().to_string_lossy().to_lowercase();
            GAME_MARKERS.contains(&name.as_str())
                || name.ends_with(".esp")
                || name.ends_with(".esm")
                || name.ends_with(".esl")
                || name.ends_with(".bsa")
        });
        if has_marker {
            return Ok(());
        }

        // Only descend automatically when there's exactly one subfolder and
        // nothing else at this level (a clear "wrapper" folder).
        let dirs: Vec<_> = entries.iter().filter(|e| e.path().is_dir()).collect();
        if entries.len() == 1 && dirs.len() == 1 {
            let inner = dirs[0].path();
            let inner_name = dirs[0].file_name().to_string_lossy().to_lowercase();

            // If the wrapper is literally called "Data", merge its contents
            // up a level and remove the wrapper; otherwise just descend our
            // "current root" pointer conceptually by moving contents up.
            let tmp = inner.with_extension("__normalizing__");
            fs::rename(&inner, &tmp)?;
            for entry in fs::read_dir(&tmp)? {
                let entry = entry?;
                let dest = dir.join(entry.file_name());
                fs::rename(entry.path(), dest)?;
            }
            fs::remove_dir_all(&tmp)?;
            let _ = inner_name; // used only for readability above
            continue;
        }

        // No obvious wrapper to unwrap and no marker found — leave as-is.
        // Installation still succeeds; the user will just see an odd layout
        // in the file conflict view and can fix it manually if needed.
        return Ok(());
    }
}

/// Extract (or copy) a source archive/folder into `dest`. Public so the
/// FOMOD installer and other multi-step flows can unpack first, then decide
/// which files to keep.
pub fn extract_archive_to(source: &Path, dest: &Path) -> Result<()> {
    if !source.exists() {
        bail!("source path does not exist: {}", source.display());
    }
    fs::create_dir_all(dest)
        .with_context(|| format!("creating extract destination {}", dest.display()))?;
    install_source_into(source, dest)
}

/// Extract/copy `source` into `content_dir`, dispatching on what `source`
/// actually is. This is the single place that decides "how do we get files
/// out of this thing", shared by both fresh installs and in-place updates.
/// Supports: an already-extracted folder, `.zip`, `.7z`, `.tar`/`.tar.gz`/
/// `.tgz`, or — for anything else — a single loose file (a lone .esp, a
/// standalone texture, a script, whatever), copied in as-is. `.rar` is
/// explicitly rejected since there's no MIT-friendly pure-Rust decoder.
fn install_source_into(source: &Path, content_dir: &Path) -> Result<()> {
    if source.is_dir() {
        return copy_dir_recursive(source, content_dir);
    }

    let lower_name = source.to_string_lossy().to_lowercase();
    let ext = source
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase());

    if lower_name.ends_with(".tar.gz") || lower_name.ends_with(".tgz") {
        return extract_tar_gz(source, content_dir);
    }

    match ext.as_deref() {
        Some("zip") => extract_zip(source, content_dir),
        Some("7z") => extract_7z(source, content_dir),
        Some("tar") => extract_tar(source, content_dir),
        Some("rar") => bail!(
            ".rar is not supported (no license-friendly pure-Rust extractor). \
             Please re-extract with 7-Zip/unrar and install the resulting folder \
             or re-pack it as .zip or .7z."
        ),
        // Anything else — a lone .esp/.esl/.esm, a single loose .dds/.tga
        // texture, a standalone .bsa, a script, whatever — gets installed
        // as a single-file mod, placed at the root of its own content dir
        // so it lands at the root of Data on deploy. This is what makes
        // "install anything" true: the store doesn't require an archive or
        // a pre-built folder structure, just a file that belongs somewhere
        // under Data.
        _ => {
            let file_name = source.file_name().context("source file has no file name")?;
            fs::copy(source, content_dir.join(file_name))
                .with_context(|| format!("copying loose file {} into store", source.display()))?;
            Ok(())
        }
    }
}

fn extract_tar(archive: &Path, dest: &Path) -> Result<()> {
    let file =
        fs::File::open(archive).with_context(|| format!("opening tar {}", archive.display()))?;
    let mut ar = tar::Archive::new(file);
    ar.unpack(dest)
        .with_context(|| format!("extracting tar {}", archive.display()))?;
    Ok(())
}

fn extract_tar_gz(archive: &Path, dest: &Path) -> Result<()> {
    let file = fs::File::open(archive)
        .with_context(|| format!("opening tar.gz {}", archive.display()))?;
    let gz = flate2::read::GzDecoder::new(file);
    let mut ar = tar::Archive::new(gz);
    ar.unpack(dest)
        .with_context(|| format!("extracting tar.gz {}", archive.display()))?;
    Ok(())
}

fn extract_zip(archive: &Path, dest: &Path) -> Result<()> {
    let file = fs::File::open(archive)
        .with_context(|| format!("opening zip {}", archive.display()))?;
    let mut zip = zip::ZipArchive::new(file)
        .with_context(|| format!("reading zip archive {}", archive.display()))?;
    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .with_context(|| format!("reading zip entry {i} from {}", archive.display()))?;
        let out_path = match entry.enclosed_name() {
            Some(p) => dest.join(p),
            None => continue, // path traversal / absolute path — skip
        };
        if entry.name().ends_with('/') {
            fs::create_dir_all(&out_path)
                .with_context(|| format!("creating zip dir {}", out_path.display()))?;
        } else {
            if let Some(parent) = out_path.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("creating zip parent {}", parent.display()))?;
            }
            let mut out_file = fs::File::create(&out_path)
                .with_context(|| format!("creating extracted file {}", out_path.display()))?;
            std::io::copy(&mut entry, &mut out_file)
                .with_context(|| format!("extracting zip member to {}", out_path.display()))?;
        }
    }
    Ok(())
}

fn extract_7z(archive: &Path, dest: &Path) -> Result<()> {
    sevenz_rust::decompress_file(archive, dest)
        .with_context(|| format!("extracting 7z {}", archive.display()))?;
    Ok(())
}

fn copy_dir_recursive(src: &Path, dest: &Path) -> Result<()> {
    for entry in walkdir::WalkDir::new(src) {
        let entry = entry.with_context(|| format!("walking {}", src.display()))?;
        let rel = entry
            .path()
            .strip_prefix(src)
            .with_context(|| format!("strip prefix {} from {}", src.display(), entry.path().display()))?;
        let target = dest.join(rel);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&target)
                .with_context(|| format!("creating directory {}", target.display()))?;
        } else if entry.file_type().is_file() {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("creating parent {}", parent.display()))?;
            }
            fs::copy(entry.path(), &target).with_context(|| {
                format!(
                    "copying {} -> {}",
                    entry.path().display(),
                    target.display()
                )
            })?;
        }
    }
    Ok(())
}

/// Preview of how large an install would be on disk.
#[derive(Debug, Clone)]
pub struct InstallSizeEstimate {
    pub bytes: u64,
    pub files: u64,
    pub source_kind: String,
}

/// Available free space on the filesystem that holds `path`, if we can
/// determine it (best-effort via `df` on Unix). Used to warn before a large
/// install — never blocks install on its own.
pub fn free_space_for(path: &Path) -> Option<u64> {
    let mut p = path.to_path_buf();
    while !p.exists() {
        if !p.pop() {
            break;
        }
    }
    let output = std::process::Command::new("df")
        .args(["-Pk", &p.to_string_lossy()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    // df -Pk: Portable format, 1024-byte blocks. Second line, 4th column = available.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.lines().nth(1)?;
    let avail_k: u64 = line.split_whitespace().nth(3)?.parse().ok()?;
    Some(avail_k.saturating_mul(1024))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_paths::AppPaths;

    fn tmp() -> (PathBuf, AppPaths) {
        let root = std::env::temp_dir().join(format!(
            "skyrim-modmgr-store-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::remove_dir_all(&root);
        let paths = AppPaths::new(root.join("app")).unwrap();
        (root, paths)
    }

    #[test]
    fn normalize_unwraps_single_wrapper() {
        let (root, _) = tmp();
        let dir = root.join("content");
        fs::create_dir_all(dir.join("CoolMod").join("meshes")).unwrap();
        fs::write(dir.join("CoolMod").join("meshes").join("a.nif"), b"x").unwrap();
        // Move CoolMod contents structure: content/CoolMod/meshes/a.nif
        normalize_root(&dir).unwrap();
        assert!(dir.join("meshes").join("a.nif").is_file());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn normalize_unwraps_data_folder() {
        let (root, _) = tmp();
        let dir = root.join("content");
        fs::create_dir_all(dir.join("Data").join("textures")).unwrap();
        fs::write(dir.join("Data").join("textures").join("a.dds"), b"x").unwrap();
        normalize_root(&dir).unwrap();
        assert!(dir.join("textures").join("a.dds").is_file());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn install_folder_and_loose_file() {
        let (root, paths) = tmp();
        let mut store = ModStore::default();

        let folder = root.join("mod");
        fs::create_dir_all(folder.join("scripts")).unwrap();
        fs::write(folder.join("scripts").join("x.pex"), b"p").unwrap();
        let id = store.install(&paths, &folder, Some("FolderMod".into())).unwrap();
        assert!(store.get(&id).unwrap().content_dir.join("scripts").join("x.pex").is_file());

        let loose = root.join("Lone.esp");
        fs::write(&loose, b"TES4").unwrap();
        let id2 = store.install(&paths, &loose, None).unwrap();
        assert!(store.get(&id2).unwrap().content_dir.join("Lone.esp").is_file());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_source_errors() {
        let (root, paths) = tmp();
        let mut store = ModStore::default();
        let err = store
            .install(&paths, &root.join("nope.zip"), None)
            .unwrap_err();
        assert!(err.to_string().contains("does not exist"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn which_mod_provides_case_insensitive() {
        let (root, paths) = tmp();
        let mut store = ModStore::default();
        let folder = root.join("mod");
        fs::create_dir_all(folder.join("textures")).unwrap();
        fs::write(folder.join("textures").join("Armor.dds"), b"x").unwrap();
        let id = store.install(&paths, &folder, Some("Tex".into())).unwrap();
        let hits = store.mods_providing_file("textures/armor.dds");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, id);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn all_tags_deduped_and_sorted() {
        let (root, paths) = tmp();
        let mut store = ModStore::default();
        let a = root.join("a");
        let b = root.join("b");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        let id_a = store.install(&paths, &a, Some("A".into())).unwrap();
        let id_b = store.install(&paths, &b, Some("B".into())).unwrap();
        store.add_tag(&paths, &id_a, "armor").unwrap();
        store.add_tag(&paths, &id_a, "textures").unwrap();
        store.add_tag(&paths, &id_b, "armor").unwrap(); // duplicate tag, different mod
        assert_eq!(store.all_tags(), vec!["armor".to_string(), "textures".to_string()]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn estimate_folder_size() {
        let (root, _) = tmp();
        let folder = root.join("mod");
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("a.bin"), vec![0u8; 100]).unwrap();
        fs::write(folder.join("b.bin"), vec![0u8; 50]).unwrap();
        let est = ModStore::estimate_install_size(&folder).unwrap();
        assert_eq!(est.bytes, 150);
        assert_eq!(est.files, 2);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn corrupt_store_json_is_repaired() {
        let (root, paths) = tmp();
        fs::write(paths.root.join("store.json"), b"{not json!!!").unwrap();
        let outcome = ModStore::load_with_repair(&paths).unwrap();
        match outcome {
            LoadOutcome::Repaired { value, .. } => assert!(value.mods.is_empty()),
            other => panic!("expected Repaired, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&root);
    }
}
