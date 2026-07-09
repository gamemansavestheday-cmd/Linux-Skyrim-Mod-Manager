use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use crate::prefix::WinePrefix;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GameEdition {
    LE, // Skyrim (2011 "Legendary Edition")
    SE, // Skyrim Special Edition
    AE, // Skyrim Anniversary Edition (SE with CC content, same layout)
    VR, // Skyrim VR
}

impl GameEdition {
    fn exe_and_appid(self) -> (&'static str, &'static str) {
        match self {
            GameEdition::LE => ("TESV.exe", "72850"),
            GameEdition::SE | GameEdition::AE => ("SkyrimSE.exe", "489830"),
            GameEdition::VR => ("SkyrimVR.exe", "611670"),
        }
    }
}

/// A located, playable Skyrim installation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameInstall {
    /// Deterministic, derived from the canonicalized install_dir path (NOT
    /// random) — this matters because backups/<id>/ and any persisted
    /// "known games" config are keyed by this id. A random id here would
    /// mean re-detecting the same game after a restart produces a *new* id,
    /// silently orphaning the vanilla-Data backup and breaking `restore`.
    pub id: String,
    pub edition: GameEdition,
    /// The game's install root (contains the exe and the `Data` folder).
    pub install_dir: PathBuf,
    /// `install_dir/Data`.
    pub data_dir: PathBuf,
    /// Where `plugins.txt` lives (Documents/My Games/Skyrim*/plugins.txt on
    /// both platforms, just reached via a different prefix on Linux).
    pub plugins_txt: PathBuf,
    /// None on native Windows; Some(prefix) when this install is inside a
    /// Wine/Proton prefix on Linux.
    #[serde(skip)]
    pub wine_prefix: Option<WinePrefix>,
}

fn derive_id(install_dir: &Path) -> String {
    let canonical = install_dir
        .canonicalize()
        .unwrap_or_else(|_| install_dir.to_path_buf());
    let mut hasher = DefaultHasher::new();
    canonical.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Search a Wine prefix's Steam library folders for a Skyrim install.
pub fn find_skyrim_in_prefix(prefix: &WinePrefix) -> Option<GameInstall> {
    // Proton prefixes: the game files live in the *host* Steam library, not
    // inside the prefix, at steamapps/common/<Game>. Try both the sibling
    // `common` dir (next to `compatdata`) and, for Lutris/custom prefixes,
    // inside `drive_c/Program Files (x86)/Steam/steamapps/common`.
    let mut candidate_roots = Vec::new();

    if let Some(compatdata) = prefix.prefix_root.parent() {
        // prefix.prefix_root is .../compatdata/<appid>/pfx
        if let Some(compatdata_parent) = compatdata.parent() {
            candidate_roots.push(compatdata_parent.join("common"));
        }
    }
    candidate_roots.push(
        prefix
            .drive_c()
            .join("Program Files (x86)")
            .join("Steam")
            .join("steamapps")
            .join("common"),
    );
    candidate_roots.push(
        prefix
            .drive_c()
            .join("Program Files")
            .join("Steam")
            .join("steamapps")
            .join("common"),
    );

    for editions in [
        (GameEdition::SE, "Skyrim Special Edition"),
        (GameEdition::LE, "Skyrim"),
        (GameEdition::VR, "SkyrimVR"),
    ] {
        let (edition, folder_name) = editions;
        for root in &candidate_roots {
            let install_dir = root.join(folder_name);
            let (exe, _) = edition.exe_and_appid();
            if install_dir.join(exe).is_file() {
                let plugins_txt = prefix
                    .my_games_dir()
                    .join(my_games_folder_name(edition))
                    .join("plugins.txt");
                return Some(GameInstall {
                    id: derive_id(&install_dir),
                    edition,
                    data_dir: install_dir.join("Data"),
                    install_dir,
                    plugins_txt,
                    wine_prefix: Some(prefix.clone()),
                });
            }
        }
    }
    None
}

/// A `GameInstall` plus extra context to help a person tell two detected
/// installs apart when more than one prefix has Skyrim in it (e.g. one
/// under Steam/Proton and another under a Lutris prefix from years ago).
#[derive(Debug, Clone)]
pub struct DetectedGame {
    pub game: GameInstall,
    /// One-line description of where this came from, e.g.
    /// "Steam/Proton — Skyrim Special Edition (appid 489830)".
    pub source_label: String,
    /// Approximate "last used" signal: the modification time of the Data
    /// folder (as seconds since Unix epoch), which changes whenever the
    /// game patches, a mod manager deploys to it, or (for the vanilla
    /// folder) it's rarely touched at all. Not a substitute for real
    /// playtime, but a useful tiebreaker: "the one you've actually been
    /// modding recently is probably this one."
    pub data_dir_modified_secs: Option<u64>,
}

/// Scan every discoverable Wine/Proton prefix (see `prefix::discover_prefixes`)
/// for a Skyrim install, returning all matches found. If this returns more
/// than one entry, the caller (CLI or GUI) should show the person the list
/// — each `source_label` and `data_dir_modified_secs` — and ask them to pick
/// one rather than silently guessing.
pub fn scan_all_prefixes_for_skyrim() -> Vec<DetectedGame> {
    let mut results = Vec::new();
    for prefix in crate::prefix::discover_prefixes() {
        if let Some(game) = find_skyrim_in_prefix(&prefix) {
            let data_dir_modified_secs = std::fs::metadata(&game.data_dir)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs());
            let source_label = game
                .wine_prefix
                .as_ref()
                .map(|p| p.source.label())
                .unwrap_or_else(|| "Native".to_string());
            results.push(DetectedGame {
                game,
                source_label,
                data_dir_modified_secs,
            });
        }
    }
    results
}


/// Locate a Skyrim install from a native path (used directly on Windows, or
/// if a Linux user points at an already-known install_dir manually).
pub fn find_skyrim_at(install_dir: &Path, my_games_root: &Path) -> Option<GameInstall> {
    for edition in [GameEdition::SE, GameEdition::LE, GameEdition::VR] {
        let (exe, _) = edition.exe_and_appid();
        if install_dir.join(exe).is_file() {
            let plugins_txt = my_games_root
                .join(my_games_folder_name(edition))
                .join("plugins.txt");
            return Some(GameInstall {
                id: derive_id(install_dir),
                edition,
                data_dir: install_dir.join("Data"),
                install_dir: install_dir.to_path_buf(),
                plugins_txt,
                wine_prefix: None,
            });
        }
    }
    None
}

fn my_games_folder_name(edition: GameEdition) -> &'static str {
    match edition {
        GameEdition::LE => "Skyrim",
        GameEdition::SE | GameEdition::AE => "Skyrim Special Edition",
        GameEdition::VR => "Skyrim VR",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn game_id_is_deterministic() {
        let root = std::env::temp_dir().join(format!(
            "skyrim-modmgr-gameid-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("SkyrimSE.exe"), b"x").unwrap();
        fs::create_dir_all(root.join("Data")).unwrap();

        let a = find_skyrim_at(&root, &root).expect("SE");
        let b = find_skyrim_at(&root, &root).expect("SE again");
        assert_eq!(a.id, b.id);
        assert_eq!(a.edition, GameEdition::SE);
        assert_eq!(a.data_dir, root.join("Data"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn derive_id_stable_for_same_path() {
        let p = PathBuf::from("/tmp/some/skyrim/install");
        assert_eq!(derive_id(&p), derive_id(&p));
    }
}
