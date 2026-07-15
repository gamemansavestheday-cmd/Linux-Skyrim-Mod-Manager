//! Save-game manager: list, import, and export Skyrim save files.
//!
//! Saves live under the game's My Games folder (`…/Skyrim Special Edition/Saves`
//! etc.). Each character save is typically a pair of `.ess` + optional `.skse`
//! co-save, plus optional screenshot images.

use anyhow::{bail, Context, Result};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::game::GameInstall;

/// One save-related file in the game's Saves folder.
#[derive(Debug, Clone)]
pub struct SaveFile {
    /// File name only (e.g. `Save 42 - MyCharacter  45.32.10.ess`).
    pub name: String,
    pub path: PathBuf,
    pub size_bytes: u64,
    /// Seconds since Unix epoch, if available.
    pub modified_secs: Option<u64>,
    /// Kind of file for UI filtering.
    pub kind: SaveKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveKind {
    /// Primary Skyrim save (`.ess`).
    Ess,
    /// SKSE co-save (`.skse`).
    Skse,
    /// Character screenshot / preview image.
    Image,
    Other,
}

impl SaveKind {
    fn from_name(name: &str) -> Self {
        let lower = name.to_lowercase();
        if lower.ends_with(".ess") {
            Self::Ess
        } else if lower.ends_with(".skse") {
            Self::Skse
        } else if lower.ends_with(".jpg")
            || lower.ends_with(".jpeg")
            || lower.ends_with(".png")
            || lower.ends_with(".bmp")
        {
            Self::Image
        } else {
            Self::Other
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Ess => "save",
            Self::Skse => "skse",
            Self::Image => "image",
            Self::Other => "other",
        }
    }
}

/// Locate the game's `Saves` directory from a [`GameInstall`].
pub fn saves_dir(game: &GameInstall) -> Option<PathBuf> {
    game.plugins_txt.parent().map(|p| p.join("Saves"))
}

/// List every file in the Saves folder (sorted newest first).
pub fn list_saves(game: &GameInstall) -> Result<Vec<SaveFile>> {
    let Some(dir) = saves_dir(game) else {
        return Ok(Vec::new());
    };
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(&dir)
        .with_context(|| format!("reading saves directory {}", dir.display()))?
        .flatten()
    {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let meta = entry.metadata().ok();
        let size_bytes = meta.as_ref().map(|m| m.len()).unwrap_or(0);
        let modified_secs = meta
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs());
        out.push(SaveFile {
            kind: SaveKind::from_name(&name),
            name,
            path,
            size_bytes,
            modified_secs,
        });
    }
    out.sort_by(|a, b| b.modified_secs.cmp(&a.modified_secs).then(a.name.cmp(&b.name)));
    Ok(out)
}

/// Export selected save files into a destination directory (preserves names).
/// If `dest` ends with `.zip` or is intended as a zip, pass `as_zip: true`.
pub fn export_saves(files: &[&SaveFile], dest: &Path, as_zip: bool) -> Result<usize> {
    if files.is_empty() {
        bail!("no save files selected to export");
    }
    if as_zip || dest.extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case("zip")).unwrap_or(false) {
        export_saves_zip(files, dest)
    } else {
        export_saves_dir(files, dest)
    }
}

fn export_saves_dir(files: &[&SaveFile], dest: &Path) -> Result<usize> {
    fs::create_dir_all(dest)
        .with_context(|| format!("creating export directory {}", dest.display()))?;
    let mut count = 0usize;
    for file in files {
        let target = dest.join(&file.name);
        fs::copy(&file.path, &target).with_context(|| {
            format!(
                "exporting {} -> {}",
                file.path.display(),
                target.display()
            )
        })?;
        count += 1;
    }
    Ok(count)
}

fn export_saves_zip(files: &[&SaveFile], dest: &Path) -> Result<usize> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating parent for {}", dest.display()))?;
    }
    let zip_file = fs::File::create(dest)
        .with_context(|| format!("creating zip {}", dest.display()))?;
    let mut zip = zip::ZipWriter::new(zip_file);
    let options = zip::write::FileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    let mut count = 0usize;
    for file in files {
        zip.start_file(&file.name, options)
            .with_context(|| format!("adding {} to zip", file.name))?;
        let mut f = fs::File::open(&file.path)
            .with_context(|| format!("reading {}", file.path.display()))?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf)?;
        zip.write_all(&buf)?;
        count += 1;
    }
    zip.finish()
        .with_context(|| format!("finalizing zip {}", dest.display()))?;
    Ok(count)
}

/// Import save files (or a `.zip` of them) into the game's Saves folder.
/// Returns how many files were written.
pub fn import_saves(game: &GameInstall, source: &Path) -> Result<usize> {
    let Some(dir) = saves_dir(game) else {
        bail!("could not determine Saves directory for this game install");
    };
    fs::create_dir_all(&dir)
        .with_context(|| format!("creating Saves directory {}", dir.display()))?;

    if !source.exists() {
        bail!("import source does not exist: {}", source.display());
    }

    if source.is_dir() {
        return import_from_dir(&dir, source);
    }

    let lower = source.to_string_lossy().to_lowercase();
    if lower.ends_with(".zip") {
        return import_from_zip(&dir, source);
    }

    // Single loose file (ess/skse/image).
    let name = source
        .file_name()
        .context("import source has no file name")?;
    let dest = dir.join(name);
    fs::copy(source, &dest).with_context(|| {
        format!(
            "importing {} -> {}",
            source.display(),
            dest.display()
        )
    })?;
    Ok(1)
}

fn import_from_dir(saves: &Path, source: &Path) -> Result<usize> {
    let mut count = 0usize;
    for entry in walkdir::WalkDir::new(source)
        .max_depth(2)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
    {
        let name = entry.file_name().to_string_lossy().to_string();
        // Only import things that look like save-related files.
        match SaveKind::from_name(&name) {
            SaveKind::Other => continue,
            _ => {}
        }
        let dest = saves.join(&name);
        fs::copy(entry.path(), &dest).with_context(|| {
            format!(
                "importing {} -> {}",
                entry.path().display(),
                dest.display()
            )
        })?;
        count += 1;
    }
    if count == 0 {
        bail!(
            "no .ess/.skse/image files found under {}",
            source.display()
        );
    }
    Ok(count)
}

fn import_from_zip(saves: &Path, archive: &Path) -> Result<usize> {
    let file = fs::File::open(archive)
        .with_context(|| format!("opening zip {}", archive.display()))?;
    let mut zip = zip::ZipArchive::new(file)
        .with_context(|| format!("reading zip {}", archive.display()))?;
    let mut count = 0usize;
    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .with_context(|| format!("reading zip entry {i}"))?;
        if entry.is_dir() {
            continue;
        }
        let Some(enclosed) = entry.enclosed_name() else {
            continue;
        };
        let name = enclosed
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        if name.is_empty() {
            continue;
        }
        match SaveKind::from_name(&name) {
            SaveKind::Other => continue,
            _ => {}
        }
        let dest = saves.join(&name);
        let mut out = fs::File::create(&dest)
            .with_context(|| format!("creating {}", dest.display()))?;
        std::io::copy(&mut entry, &mut out)
            .with_context(|| format!("extracting {} to {}", name, dest.display()))?;
        count += 1;
    }
    if count == 0 {
        bail!(
            "no .ess/.skse/image files found in {}",
            archive.display()
        );
    }
    Ok(count)
}

/// Delete selected save files from the Saves folder.
pub fn delete_saves(files: &[&SaveFile]) -> Result<usize> {
    let mut count = 0usize;
    for file in files {
        if file.path.is_file() {
            fs::remove_file(&file.path)
                .with_context(|| format!("deleting {}", file.path.display()))?;
            count += 1;
        }
    }
    Ok(count)
}
