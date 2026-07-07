use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;

use crate::app_paths::AppPaths;

/// One mod's slot within a profile's load order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileModEntry {
    pub mod_id: String,
    pub enabled: bool,
}

/// A profile: an ordered list of mods (lowest priority first, so later
/// entries win file conflicts) plus a plugin (.esp/.esm/.esl) load order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    /// Mod load order, lowest priority first. The VFS layer walks this in
    /// order so a file provided by a later mod overwrites (as a symlink
    /// target) the same file provided by an earlier one.
    pub mod_order: Vec<ProfileModEntry>,
    /// Plugin load order written out to `plugins.txt` / `loadorder.txt` on
    /// deploy. Entries are plugin file names (e.g. `Unofficial Skyrim
    /// Special Edition Patch.esp`), not mod ids, since one mod can ship
    /// multiple plugins.
    pub plugin_order: Vec<String>,
    /// Which game install (by id, see `game::GameInstall`) this profile
    /// deploys to.
    pub game_id: String,
}

impl Profile {
    pub fn new(name: impl Into<String>, game_id: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            mod_order: Vec::new(),
            plugin_order: Vec::new(),
            game_id: game_id.into(),
        }
    }

    fn file_name(name: &str) -> String {
        let safe: String = name
            .chars()
            .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
            .collect();
        format!("{safe}.json")
    }

    pub fn load(paths: &AppPaths, name: &str) -> Result<Self> {
        let file = paths.profiles.join(Self::file_name(name));
        let data = fs::read_to_string(&file)
            .with_context(|| format!("reading profile {}", file.display()))?;
        Ok(serde_json::from_str(&data)?)
    }

    pub fn save(&self, paths: &AppPaths) -> Result<()> {
        let file = paths.profiles.join(Self::file_name(&self.name));
        fs::write(&file, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn list_all(paths: &AppPaths) -> Result<Vec<String>> {
        let mut names = Vec::new();
        for entry in fs::read_dir(&paths.profiles)? {
            let entry = entry?;
            if entry.path().extension().and_then(|e| e.to_str()) == Some("json") {
                if let Ok(data) = fs::read_to_string(entry.path()) {
                    if let Ok(profile) = serde_json::from_str::<Profile>(&data) {
                        names.push(profile.name);
                    }
                }
            }
        }
        Ok(names)
    }

    /// Enable a mod, appending it to the top of the priority order (wins
    /// most conflicts) unless it's already present.
    pub fn enable_mod(&mut self, mod_id: &str) {
        if let Some(existing) = self.mod_order.iter_mut().find(|m| m.mod_id == mod_id) {
            existing.enabled = true;
        } else {
            self.mod_order.push(ProfileModEntry {
                mod_id: mod_id.to_string(),
                enabled: true,
            });
        }
    }

    pub fn disable_mod(&mut self, mod_id: &str) {
        if let Some(existing) = self.mod_order.iter_mut().find(|m| m.mod_id == mod_id) {
            existing.enabled = false;
        }
    }

    /// Enable a mod AND auto-register any .esp/.esm/.esl it ships into the
    /// plugin load order (appended at the end, i.e. loads last / highest
    /// priority among plugins — a reasonable default the person can still
    /// manually reorder). This is what makes the "extremely easy UI" goal
    /// real: installing a plugin mod and hitting "Enable" is enough to get
    /// it into `plugins.txt` on deploy, no separate step required.
    pub fn enable_mod_with_plugins(&mut self, store: &crate::store::ModStore, mod_id: &str) {
        self.enable_mod(mod_id);
        for plugin in store.discover_plugins(mod_id) {
            if !self.plugin_order.iter().any(|p| p.eq_ignore_ascii_case(&plugin)) {
                self.plugin_order.push(plugin);
            }
        }
    }

    /// Move a mod to a new position in the priority list (0 = lowest
    /// priority / loads first).
    pub fn reorder(&mut self, mod_id: &str, new_index: usize) {
        if let Some(pos) = self.mod_order.iter().position(|m| m.mod_id == mod_id) {
            let entry = self.mod_order.remove(pos);
            let idx = new_index.min(self.mod_order.len());
            self.mod_order.insert(idx, entry);
        }
    }

    pub fn enabled_mods_in_order(&self) -> impl Iterator<Item = &str> {
        self.mod_order
            .iter()
            .filter(|m| m.enabled)
            .map(|m| m.mod_id.as_str())
    }

    /// Duplicate this profile under a new name (e.g. to try something risky
    /// without touching a known-good load order). Returns the new profile.
    pub fn clone_as(&self, paths: &AppPaths, new_name: &str) -> Result<Profile> {
        let mut copy = self.clone();
        copy.name = new_name.to_string();
        copy.save(paths)?;
        Ok(copy)
    }

    /// Rename this profile in place (deletes the old file, writes the new
    /// one).
    pub fn rename(&mut self, paths: &AppPaths, new_name: &str) -> Result<()> {
        let old_file = paths.profiles.join(Self::file_name(&self.name));
        self.name = new_name.to_string();
        self.save(paths)?;
        if old_file.exists() {
            fs::remove_file(old_file)?;
        }
        Ok(())
    }

    /// Delete a profile's on-disk definition. Does not touch the mod store.
    pub fn delete(paths: &AppPaths, name: &str) -> Result<()> {
        let file = paths.profiles.join(Self::file_name(name));
        if file.exists() {
            fs::remove_file(file)?;
        }
        Ok(())
    }

    /// Export a human-readable summary (mod load order by name + plugin
    /// load order) for sharing with someone else, e.g. in a Discord message
    /// or a modlist writeup. This is intentionally plain text, not a
    /// re-importable mod pack — the receiving person still needs to obtain
    /// and install the mods themselves.
    pub fn export_readable(&self, store: &crate::store::ModStore) -> String {
        let mut out = String::new();
        out.push_str(&format!("Profile: {}\n\n", self.name));
        out.push_str("Mod load order (top = lowest priority, bottom wins conflicts):\n");
        for entry in &self.mod_order {
            let name = store
                .get(&entry.mod_id)
                .map(|m| m.name.clone())
                .unwrap_or_else(|| entry.mod_id.clone());
            let mark = if entry.enabled { "x" } else { " " };
            out.push_str(&format!("  [{mark}] {name}\n"));
        }
        out.push_str("\nPlugin load order:\n");
        for plugin in &self.plugin_order {
            out.push_str(&format!("  {plugin}\n"));
        }
        out
    }

    /// Import a plugin load order directly from an existing `plugins.txt`
    /// (e.g. one exported by MO2/Vortex, or a previous install), replacing
    /// this profile's current plugin order. Lines starting with `#` are
    /// comments; a leading `*` (Skyrim SE's "enabled" marker) is stripped
    /// since we regenerate that marker ourselves on deploy.
    pub fn import_plugins_txt(&mut self, path: &std::path::Path) -> Result<()> {
        let contents = fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        self.plugin_order = contents
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(|l| l.trim_start_matches('*').to_string())
            .collect();
        Ok(())
    }
}
