use anyhow::{Context, Result};
use std::os::unix::fs::symlink;
use std::path::Path;

use super::LinkBackend;

/// Linux backend: plain symlinks everywhere. Symlinks work correctly across
/// Wine/Proton prefixes because Wine resolves them at the host filesystem
/// level before the Windows game process ever sees a path — from the game's
/// point of view, the file is just there.
pub struct LinuxBackend;

impl LinkBackend for LinuxBackend {
    fn link_file(&self, source: &Path, dest: &Path) -> Result<()> {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating staging dir {}", parent.display()))?;
        }
        if dest.exists() || dest.symlink_metadata().is_ok() {
            std::fs::remove_file(dest)
                .with_context(|| format!("removing stale link {}", dest.display()))?;
        }
        symlink(source, dest)
            .with_context(|| format!("symlinking {} -> {}", dest.display(), source.display()))?;
        Ok(())
    }

    fn mount_staging_over_data(
        &self,
        staging_dir: &Path,
        data_dir: &Path,
        backup_dir: &Path,
    ) -> Result<()> {
        // Back up the real Data folder exactly once. If `data_dir` is
        // already our own symlink (from a previous deploy), there's nothing
        // to back up — just replace the link.
        let is_our_link = data_dir
            .symlink_metadata()
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false);

        if data_dir.exists() && !is_our_link {
            if let Some(parent) = backup_dir.parent() {
                std::fs::create_dir_all(parent)?;
            }
            if !backup_dir.exists() {
                std::fs::rename(data_dir, backup_dir).with_context(|| {
                    format!(
                        "backing up original Data folder {} -> {}",
                        data_dir.display(),
                        backup_dir.display()
                    )
                })?;
            }
        } else if is_our_link {
            std::fs::remove_file(data_dir)?;
        }

        if let Some(parent) = data_dir.parent() {
            std::fs::create_dir_all(parent)?;
        }
        symlink(staging_dir, data_dir).with_context(|| {
            format!(
                "mounting staging {} over Data {}",
                staging_dir.display(),
                data_dir.display()
            )
        })?;
        Ok(())
    }

    fn unmount(&self, data_dir: &Path, backup_dir: &Path) -> Result<()> {
        let is_our_link = data_dir
            .symlink_metadata()
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false);
        if is_our_link {
            std::fs::remove_file(data_dir)?;
        }
        if backup_dir.exists() {
            std::fs::rename(backup_dir, data_dir)?;
        }
        Ok(())
    }
}
