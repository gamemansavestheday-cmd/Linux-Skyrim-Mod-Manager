use anyhow::{Context, Result};
use std::path::PathBuf;

/// All the directories the app owns on disk. Nothing here ever touches the
/// game install except through the `vfs` module's deploy step.
#[derive(Debug, Clone)]
pub struct AppPaths {
    /// Root data directory, e.g. `~/.local/share/skyrim-modmgr` on Linux or
    /// `%APPDATA%\skyrim-modmgr` on Windows.
    pub root: PathBuf,
    /// Where extracted mod contents live, one subfolder per mod id.
    pub mods: PathBuf,
    /// Where profile definitions (JSON) live.
    pub profiles: PathBuf,
    /// Scratch space for extracting archives before they're moved into `mods`.
    pub tmp: PathBuf,
    /// Where the "backup of the original Data folder" is kept, so we can
    /// always restore the game to a vanilla state.
    pub backups: PathBuf,
    /// App config (known game installs / prefixes, active profile, etc).
    pub config_file: PathBuf,
}

impl AppPaths {
    pub fn discover() -> Result<Self> {
        let root = dirs::data_dir()
            .context("could not determine platform data directory")?
            .join("skyrim-modmgr");
        Self::new(root)
    }

    pub fn new(root: PathBuf) -> Result<Self> {
        let mods = root.join("mods");
        let profiles = root.join("profiles");
        let tmp = root.join("tmp");
        let backups = root.join("backups");
        let config_file = root.join("config.json");

        for dir in [&root, &mods, &profiles, &tmp, &backups] {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("creating app directory {}", dir.display()))?;
        }

        Ok(Self {
            root,
            mods,
            profiles,
            tmp,
            backups,
            config_file,
        })
    }
}
