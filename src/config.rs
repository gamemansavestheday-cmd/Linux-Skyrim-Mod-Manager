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
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    pub known_games: Vec<GameInstall>,
    pub active_game_id: Option<String>,
    pub active_profile: Option<String>,
}

impl Config {
    pub fn load(paths: &AppPaths) -> Result<Self> {
        if !paths.config_file.exists() {
            return Ok(Self::default());
        }
        let data = fs::read_to_string(&paths.config_file)
            .with_context(|| format!("reading {}", paths.config_file.display()))?;
        Ok(serde_json::from_str(&data).unwrap_or_default())
    }

    pub fn save(&self, paths: &AppPaths) -> Result<()> {
        fs::write(&paths.config_file, serde_json::to_string_pretty(self)?)?;
        Ok(())
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
}
