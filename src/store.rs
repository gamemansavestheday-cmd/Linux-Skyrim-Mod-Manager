use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::app_paths::AppPaths;

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
        let manifest = paths.root.join(MANIFEST_FILE);
        if !manifest.exists() {
            return Ok(Self::default());
        }
        let data = fs::read_to_string(&manifest)
            .with_context(|| format!("reading {}", manifest.display()))?;
        Ok(serde_json::from_str(&data)?)
    }

    pub fn save(&self, paths: &AppPaths) -> Result<()> {
        let manifest = paths.root.join(MANIFEST_FILE);
        let data = serde_json::to_string_pretty(self)?;
        fs::write(&manifest, data)?;
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
        if !source.exists() {
            bail!("source path does not exist: {}", source.display());
        }

        let id = Uuid::new_v4().to_string();
        let content_dir = paths.mods.join(&id);
        fs::create_dir_all(&content_dir)?;

        install_source_into(source, &content_dir)?;
        normalize_root(&content_dir)?;

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
        Ok(id)
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
            let entry = self.get(id).context("no such mod id")?;
            entry.content_dir.clone()
        };
        if content_dir.exists() {
            fs::remove_dir_all(&content_dir)?;
        }
        fs::create_dir_all(&content_dir)?;
        install_source_into(source, &content_dir)?;
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
    let mut zip = zip::ZipArchive::new(file)?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        let out_path = match entry.enclosed_name() {
            Some(p) => dest.join(p),
            None => continue,
        };
        if entry.name().ends_with('/') {
            fs::create_dir_all(&out_path)?;
        } else {
            if let Some(parent) = out_path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut out_file = fs::File::create(&out_path)?;
            std::io::copy(&mut entry, &mut out_file)?;
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
        let entry = entry?;
        let rel = entry.path().strip_prefix(src)?;
        let target = dest.join(rel);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&target)?;
        } else if entry.file_type().is_file() {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}
