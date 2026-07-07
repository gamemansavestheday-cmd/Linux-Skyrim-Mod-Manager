use anyhow::{Context, Result};
use std::path::Path;

use super::LinkBackend;

/// Windows backend. True symlinks (`std::os::windows::fs::symlink_file`)
/// require either Administrator or Developer Mode enabled, which we can't
/// assume a modder has. Instead we use:
///   - **Hardlinks** for individual files (`CreateHardLink`, no special
///     privileges, but requires source and dest to be on the same NTFS
///     volume — true for the overwhelming majority of setups where the mod
///     store and the game live on the same drive).
///   - **NTFS directory junctions** (`fsutil reparsepoint` equivalent, via
///     the `junction` crate) for mounting the whole staging `Data` tree over
///     the game's real `Data` folder — also no special privileges required,
///     also same-volume only.
///
/// If mod store and game install end up on different drives, hardlinking
/// fails; in that case we fall back to a plain file copy for that file (at
/// the cost of extra disk space and the file no longer being "live" if the
/// source is edited) so installation still succeeds rather than erroring
/// out completely.
pub struct WindowsBackend;

impl LinkBackend for WindowsBackend {
    fn link_file(&self, source: &Path, dest: &Path) -> Result<()> {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating staging dir {}", parent.display()))?;
        }
        if dest.exists() {
            std::fs::remove_file(dest)
                .with_context(|| format!("removing stale link {}", dest.display()))?;
        }
        match std::fs::hard_link(source, dest) {
            Ok(()) => Ok(()),
            Err(_) => {
                // Different volume or filesystem that doesn't support
                // hardlinks — fall back to a copy so the deploy still
                // succeeds.
                std::fs::copy(source, dest).with_context(|| {
                    format!(
                        "hardlink failed and fallback copy also failed: {} -> {}",
                        source.display(),
                        dest.display()
                    )
                })?;
                Ok(())
            }
        }
    }

    fn mount_staging_over_data(
        &self,
        staging_dir: &Path,
        data_dir: &Path,
        backup_dir: &Path,
    ) -> Result<()> {
        let is_our_junction = junction::exists(data_dir).unwrap_or(false);

        if data_dir.exists() && !is_our_junction {
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
        } else if is_our_junction {
            std::fs::remove_dir(data_dir)?; // removing a junction point removes only the link
        }

        if let Some(parent) = data_dir.parent() {
            std::fs::create_dir_all(parent)?;
        }
        junction::create(staging_dir, data_dir).with_context(|| {
            format!(
                "creating junction {} -> {}",
                data_dir.display(),
                staging_dir.display()
            )
        })?;
        Ok(())
    }

    fn unmount(&self, data_dir: &Path, backup_dir: &Path) -> Result<()> {
        if junction::exists(data_dir).unwrap_or(false) {
            std::fs::remove_dir(data_dir)?;
        }
        if backup_dir.exists() {
            std::fs::rename(backup_dir, data_dir)?;
        }
        Ok(())
    }
}
