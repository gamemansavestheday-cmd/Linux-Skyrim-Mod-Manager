use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;

use crate::app_paths::AppPaths;
use crate::game::GameInstall;

/// Persisted app state: every game install the person has confirmed (via
/// `detect-game` + picking one when multiple were found), which one is
/// currently active, and which profile is currently active for it. This is
/// what lets `deploy` be a one-word command after the first setup, instead
/// of requiring `--install-dir`/`--my-games-dir` every single time.
#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
    pub known_games: Vec<GameInstall>,
    pub active_game_id: Option<String>,
    pub active_profile: Option<String>,
    /// Extra folders to check for a Skyrim install alongside the usual
    /// Wine/Proton/PortProton/Lutris/etc. prefixes and the default
    /// Downloads/Games/Desktop scan — for anyone whose copy lives
    /// somewhere non-standard (a second drive, a portable install, a
    /// DRM-free copy extracted by hand, etc). Persisted so it only needs
    /// to be added once.
    #[serde(default)]
    pub extra_search_paths: Vec<std::path::PathBuf>,
    /// Set after the user has seen the one-time Backup explainer dialog so
    /// we only show it the first time they press Backup.
    #[serde(default)]
    pub backup_intro_seen: bool,
    /// Last-used "include mods" toggle for the Backup tab.
    #[serde(default = "default_true")]
    pub backup_include_mods: bool,
    /// Last-used "include saves" toggle for the Backup tab.
    #[serde(default = "default_true")]
    pub backup_include_saves: bool,
}

fn default_true() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Self {
            known_games: Vec::new(),
            active_game_id: None,
            active_profile: None,
            extra_search_paths: Vec::new(),
            backup_intro_seen: false,
            backup_include_mods: true,
            backup_include_saves: true,
        }
    }
}

/// Outcome of loading config when the on-disk file may be corrupt.
#[derive(Debug)]
pub enum LoadOutcome<T> {
    /// Loaded cleanly.
    Ok(T),
    /// File was missing — using defaults.
    Missing(T),
    /// File existed but was corrupt; defaults returned and the broken file
    /// was renamed aside (see `backup_path`) so nothing is lost.
    Repaired { value: T, backup_path: std::path::PathBuf },
}

impl Config {
    pub fn load(paths: &AppPaths) -> Result<Self> {
        match Self::load_with_repair(paths)? {
            LoadOutcome::Ok(c) | LoadOutcome::Missing(c) => Ok(c),
            LoadOutcome::Repaired { value, backup_path } => {
                eprintln!(
                    "warning: config.json was corrupt and has been reset to defaults. \
                     Broken file kept at {} — re-run detect-game to re-register installs.",
                    backup_path.display()
                );
                Ok(value)
            }
        }
    }

    /// Load config, and if the JSON is unparseable, quarantine the broken
    /// file and return defaults instead of crashing the whole CLI/GUI.
    pub fn load_with_repair(paths: &AppPaths) -> Result<LoadOutcome<Self>> {
        if !paths.config_file.exists() {
            return Ok(LoadOutcome::Missing(Self::default()));
        }
        let data = fs::read_to_string(&paths.config_file)
            .with_context(|| format!("reading config file {}", paths.config_file.display()))?;
        match serde_json::from_str::<Self>(&data) {
            Ok(c) => Ok(LoadOutcome::Ok(c)),
            Err(e) => {
                let backup = paths.root.join(format!(
                    "config.json.corrupt.{}",
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0)
                ));
                fs::rename(&paths.config_file, &backup).with_context(|| {
                    format!(
                        "quarantining corrupt config {} -> {} (parse error: {e})",
                        paths.config_file.display(),
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
        let data = serde_json::to_string_pretty(self)
            .context("serializing config.json")?;
        fs::write(&paths.config_file, data)
            .with_context(|| format!("writing config file {}", paths.config_file.display()))?;
        Ok(())
    }

    /// Validate structural sanity of a loaded config (dangling active_game_id,
    /// empty install paths, etc). Returns human-readable problems; empty = ok.
    pub fn validate(&self) -> Vec<String> {
        let mut problems = Vec::new();
        if let Some(id) = &self.active_game_id {
            if !self.known_games.iter().any(|g| &g.id == id) {
                problems.push(format!(
                    "active_game_id '{id}' does not match any known game — run detect-game or use-game"
                ));
            }
        }
        for g in &self.known_games {
            if g.install_dir.as_os_str().is_empty() {
                problems.push(format!("known game {} has empty install_dir", g.id));
            }
            if g.id.is_empty() {
                problems.push("a known game has an empty id".into());
            }
        }
        problems
    }

    /// Drop dangling active_game_id references so the config is usable again.
    pub fn repair_in_memory(&mut self) {
        if let Some(id) = &self.active_game_id {
            if !self.known_games.iter().any(|g| &g.id == id) {
                self.active_game_id = self.known_games.first().map(|g| g.id.clone());
            }
        }
    }

    /// Remember a confirmed game install (or update it if we already knew
    /// about an install at the same `install_dir`), and make it active.
    pub fn remember_game(&mut self, game: GameInstall) {
        if let Some(existing) = self
            .known_games
            .iter_mut()
            .find(|g| g.install_dir == game.install_dir)
        {
            let id = existing.id.clone();
            *existing = game;
            existing.id = id;
            self.active_game_id = Some(existing.id.clone());
        } else {
            self.active_game_id = Some(game.id.clone());
            self.known_games.push(game);
        }
    }

    pub fn active_game(&self) -> Option<&GameInstall> {
        let id = self.active_game_id.as_ref()?;
        self.known_games.iter().find(|g| &g.id == id)
    }

    /// Add a custom folder to scan for a Skyrim install. No-op (not an
    /// error) if it's already present, so re-running the add command is
    /// harmless.
    pub fn add_search_path(&mut self, path: std::path::PathBuf) {
        if !self.extra_search_paths.contains(&path) {
            self.extra_search_paths.push(path);
        }
    }

    pub fn remove_search_path(&mut self, path: &std::path::Path) {
        self.extra_search_paths.retain(|p| p != path);
    }
}
