use serde::{Deserialize, Serialize};
use std::collections::{HashSet, hash_map::DefaultHasher};
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
    pub fn exe_and_appid(self) -> (&'static str, &'static str) {
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
    /// The game's install root (contains the exe and/or the `Data` folder).
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

/// Official + common launcher executables that identify an edition.
const SE_EXES: &[&str] = &[
    "SkyrimSE.exe",
    "SkyrimSELauncher.exe",
    "skse64_loader.exe",
    "skse64_loader",
];
const LE_EXES: &[&str] = &["TESV.exe", "SkyrimLauncher.exe", "skse_loader.exe", "skse_loader"];
const VR_EXES: &[&str] = &["SkyrimVR.exe", "sksevr_loader.exe"];

/// Directory names we never descend into during deep scans (case-insensitive).
const SKIP_DIR_NAMES: &[&str] = &[
    ".git",
    ".svn",
    ".hg",
    "node_modules",
    "__pycache__",
    ".cache",
    "cache",
    "caches",
    ".npm",
    ".cargo",
    ".rustup",
    ".local", // too broad under home; we still scan known subpaths explicitly
    "proc",
    "sys",
    "dev",
    "lost+found",
    "windows",
    "winsxs",
    "system32",
    "syswow64",
    "$recycle.bin",
    "recycle.bin",
    "trash",
    ".trash",
    "tmp",
    "temp",
    "shadercache",
    "shader cache",
    "steamapps/downloading",
    "compatdata", // prefixes are scanned separately; skip re-walking pfx trees from host libs
];

/// Quick check: does this directory look like a Skyrim install root?
/// Accepts official exes, SKSE loaders, or a `Data` folder that clearly
/// belongs to Skyrim (Skyrim.esm / Update.esm / common BSAs). This is what
/// makes non-Steam / renamed / "portable" layouts work.
pub fn identify_skyrim_at(dir: &Path) -> Option<GameEdition> {
    if !dir.is_dir() {
        return None;
    }

    // Path might be the Data folder itself.
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if name == "data" {
        if data_folder_looks_like_skyrim(dir) {
            // Prefer parent's edition from sibling exes.
            if let Some(parent) = dir.parent() {
                return Some(edition_from_exes(parent).unwrap_or(GameEdition::SE));
            }
            return Some(GameEdition::SE);
        }
        return None;
    }

    if let Some(ed) = edition_from_exes(dir) {
        // Prefer an install that also has Data when both exist nearby,
        // but an exe alone is enough.
        return Some(ed);
    }

    let data = dir.join("Data");
    if data_folder_looks_like_skyrim(&data) {
        return Some(GameEdition::SE);
    }
    // Case-insensitive Data/
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false)
                && e.file_name().to_string_lossy().eq_ignore_ascii_case("data")
                && data_folder_looks_like_skyrim(&e.path())
            {
                return Some(GameEdition::SE);
            }
        }
    }
    None
}

fn edition_from_exes(dir: &Path) -> Option<GameEdition> {
    if any_file_exists(dir, VR_EXES) {
        return Some(GameEdition::VR);
    }
    if any_file_exists(dir, SE_EXES) {
        return Some(GameEdition::SE);
    }
    if any_file_exists(dir, LE_EXES) {
        return Some(GameEdition::LE);
    }
    // Case-insensitive scan for the common names (Wine on Linux can be mixed).
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            if !e.file_type().map(|t| t.is_file()).unwrap_or(false) {
                continue;
            }
            let n = e.file_name().to_string_lossy().to_lowercase();
            if n == "skyrimvr.exe" || n == "sksevr_loader.exe" {
                return Some(GameEdition::VR);
            }
            if n == "skyrimse.exe"
                || n == "skyrimselauncher.exe"
                || n == "skse64_loader.exe"
                || n == "skse64_loader"
            {
                return Some(GameEdition::SE);
            }
            if n == "tesv.exe"
                || n == "skyrimlauncher.exe"
                || n == "skse_loader.exe"
                || n == "skse_loader"
            {
                return Some(GameEdition::LE);
            }
        }
    }
    None
}

fn any_file_exists(dir: &Path, names: &[&str]) -> bool {
    names.iter().any(|n| dir.join(n).is_file())
}

/// True if `data_dir` contains files that basically only Skyrim ships.
fn data_folder_looks_like_skyrim(data_dir: &Path) -> bool {
    if !data_dir.is_dir() {
        return false;
    }
    // Case-sensitive fast path.
    for marker in [
        "Skyrim.esm",
        "Update.esm",
        "Skyrim - Interface.bsa",
        "Skyrim - Meshes0.bsa",
        "Skyrim - Meshes.bsa",
        "Skyrim - Textures0.bsa",
        "Skyrim - Textures.bsa",
    ] {
        if data_dir.join(marker).is_file() {
            return true;
        }
    }
    // Case-insensitive fallback (common on copies from Windows).
    let Ok(entries) = std::fs::read_dir(data_dir) else {
        return false;
    };
    for e in entries.flatten() {
        let n = e.file_name().to_string_lossy().to_lowercase();
        if n == "skyrim.esm"
            || n == "update.esm"
            || n.starts_with("skyrim - ") && n.ends_with(".bsa")
            || n == "dawnguard.esm"
            || n == "hearthfires.esm"
            || n == "dragonborn.esm"
        {
            return true;
        }
    }
    false
}

fn resolve_data_dir(install_dir: &Path) -> PathBuf {
    let direct = install_dir.join("Data");
    if direct.is_dir() {
        return direct;
    }
    if let Ok(entries) = std::fs::read_dir(install_dir) {
        for e in entries.flatten() {
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false)
                && e.file_name().to_string_lossy().eq_ignore_ascii_case("data")
            {
                return e.path();
            }
        }
    }
    direct
}

/// Normalize a user-supplied path: if they pointed at `Data/`, walk up one.
pub fn normalize_install_dir(path: &Path) -> PathBuf {
    if path
        .file_name()
        .map(|n| n.to_string_lossy().eq_ignore_ascii_case("data"))
        .unwrap_or(false)
        && data_folder_looks_like_skyrim(path)
    {
        if let Some(parent) = path.parent() {
            return parent.to_path_buf();
        }
    }
    path.to_path_buf()
}

fn build_game(
    install_dir: PathBuf,
    edition: GameEdition,
    wine_prefix: Option<WinePrefix>,
    my_games_override: Option<&Path>,
) -> GameInstall {
    let install_dir = normalize_install_dir(&install_dir);
    let data_dir = resolve_data_dir(&install_dir);
    let my_games = if let Some(p) = my_games_override {
        p.to_path_buf()
    } else if let Some(ref prefix) = wine_prefix {
        prefix.my_games_dir()
    } else {
        // Host Documents/My Games when available; else install dir (best-effort).
        dirs::document_dir()
            .map(|d| d.join("My Games"))
            .unwrap_or_else(|| install_dir.clone())
    };
    let plugins_txt = my_games
        .join(my_games_folder_name(edition))
        .join("plugins.txt");
    GameInstall {
        id: derive_id(&install_dir),
        edition,
        data_dir,
        install_dir,
        plugins_txt,
        wine_prefix,
    }
}

/// Search a Wine prefix for a Skyrim install. Looks in Steam `common`
/// siblings *and* deep-walks `drive_c` (and a few host-adjacent spots)
/// so non-Steam / relocated / portable copies still show up.
pub fn find_skyrim_in_prefix(prefix: &WinePrefix) -> Option<GameInstall> {
    let found = find_all_skyrim_in_prefix(prefix);
    found.into_iter().next()
}

/// All Skyrim installs reachable from one prefix (some people have LE+SE).
pub fn find_all_skyrim_in_prefix(prefix: &WinePrefix) -> Vec<GameInstall> {
    let mut hits: Vec<PathBuf> = Vec::new();
    let mut seen = HashSet::new();

    // Steam/Proton layout: game files live on the host next to compatdata.
    let mut candidate_roots = Vec::new();
    if let Some(compatdata) = prefix.prefix_root.parent() {
        // .../compatdata/<appid>/pfx  →  .../common
        if let Some(compatdata_parent) = compatdata.parent() {
            candidate_roots.push(compatdata_parent.join("common"));
            // Also walk the whole steamapps tree shallowly.
            candidate_roots.push(compatdata_parent.to_path_buf());
        }
    }
    // Inside the prefix.
    candidate_roots.push(prefix.drive_c());
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
    // Common "I just dumped the game here" locations inside Wine.
    for extra in ["Games", "Program Files/Games", "users/Public/Games", "GOG Games"] {
        candidate_roots.push(prefix.drive_c().join(extra));
    }

    for root in &candidate_roots {
        if !root.exists() {
            continue;
        }
        // Named Steam folders first (fast path).
        for (edition, folder_name) in [
            (GameEdition::SE, "Skyrim Special Edition"),
            (GameEdition::LE, "Skyrim"),
            (GameEdition::VR, "SkyrimVR"),
        ] {
            let install_dir = root.join(folder_name);
            if identify_skyrim_at(&install_dir).is_some() || edition_from_exes(&install_dir).is_some()
            {
                push_unique_path(&mut hits, &mut seen, install_dir);
                let _ = edition;
            }
        }
        // Deep walk — depth high enough for nested "Games/RPG/Skyrim SE/…" dumps.
        let depth = if root == &prefix.drive_c() { 10 } else { 6 };
        for install in scan_tree_for_install_dirs(root, depth) {
            push_unique_path(&mut hits, &mut seen, install);
        }
    }

    hits.into_iter()
        .filter_map(|dir| {
            let edition = identify_skyrim_at(&dir)?;
            Some(build_game(
                dir,
                edition,
                Some(prefix.clone()),
                Some(&prefix.my_games_dir()),
            ))
        })
        .collect()
}

fn push_unique_path(hits: &mut Vec<PathBuf>, seen: &mut HashSet<String>, path: PathBuf) {
    let key = path
        .canonicalize()
        .unwrap_or_else(|_| path.clone())
        .to_string_lossy()
        .to_lowercase();
    if seen.insert(key) {
        hits.push(path);
    }
}

/// Walk a directory tree looking for Skyrim install roots.
fn scan_tree_for_install_dirs(root: &Path, max_depth: usize) -> Vec<PathBuf> {
    let mut results = Vec::new();
    let mut seen = HashSet::new();
    if !root.is_dir() {
        return results;
    }

    let walker = walkdir::WalkDir::new(root)
        .max_depth(max_depth)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            if e.depth() == 0 {
                return true;
            }
            let name = e.file_name().to_string_lossy();
            !should_skip_dir_name(&name)
        });

    for entry in walker.filter_map(|e| e.ok()).filter(|e| e.file_type().is_dir()) {
        let dir = entry.path();

        // Hitting a Data folder that looks like Skyrim → parent is install.
        let fname = dir
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if fname == "data" && data_folder_looks_like_skyrim(dir) {
            if let Some(parent) = dir.parent() {
                push_unique_path(&mut results, &mut seen, parent.to_path_buf());
            }
            continue;
        }

        if identify_skyrim_at(dir).is_some() {
            push_unique_path(&mut results, &mut seen, dir.to_path_buf());
        }
    }
    results
}

fn should_skip_dir_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    SKIP_DIR_NAMES.iter().any(|s| *s == lower.as_str())
        || lower.ends_with(".app")
        || lower.starts_with('.') && lower != ".wine" && lower != ".steam"
}

/// A `GameInstall` plus extra context to help a person tell two detected
/// installs apart when more than one prefix has Skyrim in it.
#[derive(Debug, Clone)]
pub struct DetectedGame {
    pub game: GameInstall,
    /// One-line description of where this came from, e.g.
    /// "Steam/Proton — Skyrim Special Edition (appid 489830)".
    pub source_label: String,
    /// Approximate "last used" signal: the modification time of the Data
    /// folder (as seconds since Unix epoch).
    pub data_dir_modified_secs: Option<u64>,
}

fn to_detected(game: GameInstall, source_label: String) -> DetectedGame {
    let data_dir_modified_secs = std::fs::metadata(&game.data_dir)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs());
    DetectedGame {
        game,
        source_label,
        data_dir_modified_secs,
    }
}

fn dedupe_detected(mut results: Vec<DetectedGame>) -> Vec<DetectedGame> {
    let mut seen = HashSet::new();
    results.retain(|d| {
        let key = d
            .game
            .install_dir
            .canonicalize()
            .unwrap_or_else(|_| d.game.install_dir.clone())
            .to_string_lossy()
            .to_lowercase();
        seen.insert(key)
    });
    results
}

/// Scan every discoverable Wine/Proton prefix for Skyrim installs.
pub fn scan_all_prefixes_for_skyrim() -> Vec<DetectedGame> {
    let mut results = Vec::new();
    for prefix in crate::prefix::discover_prefixes() {
        for game in find_all_skyrim_in_prefix(&prefix) {
            let source_label = game
                .wine_prefix
                .as_ref()
                .map(|p| p.source.label())
                .unwrap_or_else(|| "Native".to_string());
            results.push(to_detected(game, source_label));
        }
    }
    dedupe_detected(results)
}

/// Default folders (relative to $HOME) worth checking for non-prefix installs.
const DEFAULT_SCAN_FOLDERS: &[&str] = &[
    "Downloads",
    "Games",
    "Desktop",
    "Documents",
    "bin",
    "opt",
    "skyrim",
    "Skyrim",
    "Skyrim Special Edition",
    "GOG Games",
    "Epic Games",
];

/// Search a single directory tree (bounded depth) for a Skyrim install.
pub fn scan_folder_for_skyrim(root: &Path, max_depth: usize) -> Vec<DetectedGame> {
    let mut results = Vec::new();
    for install_dir in scan_tree_for_install_dirs(root, max_depth) {
        if let Some(edition) = identify_skyrim_at(&install_dir) {
            let game = build_game(install_dir.clone(), edition, None, None);
            results.push(to_detected(
                game,
                format!("Found at {}", install_dir.display()),
            ));
        }
    }
    results
}

/// Scan default common folders plus any custom paths from config.
pub fn scan_custom_locations(extra_search_paths: &[PathBuf]) -> Vec<DetectedGame> {
    let mut results = Vec::new();
    if let Some(home) = dirs::home_dir() {
        for folder in DEFAULT_SCAN_FOLDERS {
            results.extend(scan_folder_for_skyrim(&home.join(folder), 5));
        }
        // Flatpak / portable dumps sometimes sit directly under home.
        results.extend(scan_folder_for_skyrim(&home, 2));
    }
    for path in extra_search_paths {
        results.extend(scan_folder_for_skyrim(path, 8));
    }
    // Mounted drives (USB, second disks, external libraries).
    for media_root in [
        PathBuf::from("/mnt"),
        PathBuf::from("/media"),
        PathBuf::from("/run/media"),
    ] {
        if media_root.is_dir() {
            results.extend(scan_folder_for_skyrim(&media_root, 6));
        }
    }
    dedupe_detected(results)
}

/// Options for an aggressive system-wide (or root-scoped) deep search.
#[derive(Debug, Clone)]
pub struct DeepScanOptions {
    /// Roots to start from. Empty → sensible defaults (home, mounts, common
    /// game dirs, every known Wine prefix's drive_c).
    pub roots: Vec<PathBuf>,
    /// Max directory depth from each root.
    pub max_depth: usize,
    /// Also discover prefixes more aggressively and scan inside them.
    pub include_prefixes: bool,
}

impl Default for DeepScanOptions {
    fn default() -> Self {
        Self {
            roots: Vec::new(),
            max_depth: 12,
            include_prefixes: true,
        }
    }
}

/// Aggressive scan: any Wine prefix we can find, plus deep walks of home /
/// mounts / user-supplied roots. Intended for "detect still found nothing"
/// and non-standard layouts (second drive, portable folder, etc.).
pub fn deep_scan_for_skyrim(opts: &DeepScanOptions) -> Vec<DetectedGame> {
    let mut results = Vec::new();

    if opts.include_prefixes {
        // Prefer the expanded prefix discovery (includes free-floating drive_c).
        for prefix in crate::prefix::discover_prefixes_deep() {
            for game in find_all_skyrim_in_prefix(&prefix) {
                let label = format!(
                    "{} (deep)",
                    game.wine_prefix
                        .as_ref()
                        .map(|p| p.source.label())
                        .unwrap_or_else(|| "prefix".into())
                );
                results.push(to_detected(game, label));
            }
        }
    }

    let roots: Vec<PathBuf> = if opts.roots.is_empty() {
        default_deep_roots()
    } else {
        opts.roots.clone()
    };

    for root in &roots {
        for install_dir in scan_tree_for_install_dirs(root, opts.max_depth) {
            if let Some(edition) = identify_skyrim_at(&install_dir) {
                // Attach nearest enclosing prefix if we can (for My Games path).
                let prefix = crate::prefix::find_prefix_containing(&install_dir);
                let game = build_game(
                    install_dir.clone(),
                    edition,
                    prefix.clone(),
                    prefix.as_ref().map(|p| p.my_games_dir()).as_deref(),
                );
                let label = match &prefix {
                    Some(p) => format!("{} — {}", p.source.label(), install_dir.display()),
                    None => format!("Deep scan — {}", install_dir.display()),
                };
                results.push(to_detected(game, label));
            }
        }
    }

    dedupe_detected(results)
}

fn default_deep_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = dirs::home_dir() {
        roots.push(home.clone());
        for sub in [
            "Games",
            "Downloads",
            "Desktop",
            "Documents",
            ".steam",
            ".local/share/Steam",
            ".wine",
            ".var/app",
            "PortProton",
            ".PortProton",
        ] {
            let p = home.join(sub);
            if p.is_dir() {
                roots.push(p);
            }
        }
    }
    for p in [
        "/mnt",
        "/media",
        "/run/media",
        "/opt",
        "/usr/local/games",
        "/var/games",
    ] {
        let pb = PathBuf::from(p);
        if pb.is_dir() {
            roots.push(pb);
        }
    }
    roots
}

/// Full normal scan: known prefixes + default/custom folders.
pub fn scan_all_locations(extra_search_paths: &[PathBuf]) -> Vec<DetectedGame> {
    let mut results = scan_all_prefixes_for_skyrim();
    results.extend(scan_custom_locations(extra_search_paths));
    dedupe_detected(results)
}

/// Locate a Skyrim install from a path (install root *or* its Data folder).
/// Used by `--path` and the GUI manual picker.
pub fn find_skyrim_at(install_dir: &Path, my_games_root: &Path) -> Option<GameInstall> {
    let install_dir = normalize_install_dir(install_dir);
    let edition = identify_skyrim_at(&install_dir)?;
    let prefix = crate::prefix::find_prefix_containing(&install_dir);
    Some(build_game(
        install_dir,
        edition,
        prefix,
        Some(my_games_root),
    ))
}

/// Like `find_skyrim_at`, but if `my_games_root` is the install itself
/// (common when the user only picked the game folder), try Documents and
/// any enclosing Wine prefix for a better plugins.txt location.
pub fn find_skyrim_at_smart(path: &Path) -> Option<GameInstall> {
    let install_dir = normalize_install_dir(path);
    let edition = identify_skyrim_at(&install_dir)?;
    let prefix = crate::prefix::find_prefix_containing(&install_dir);
    let my_games = prefix
        .as_ref()
        .map(|p| p.my_games_dir())
        .or_else(|| dirs::document_dir().map(|d| d.join("My Games")))
        .unwrap_or_else(|| install_dir.clone());
    Some(build_game(install_dir, edition, prefix, Some(&my_games)))
}

pub fn my_games_folder_name(edition: GameEdition) -> &'static str {
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

    fn tmp() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "skyrim-modmgr-game-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn game_id_is_deterministic() {
        let root = tmp();
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
    fn detects_by_data_skyrim_esm_only() {
        let root = tmp();
        let data = root.join("Data");
        fs::create_dir_all(&data).unwrap();
        fs::write(data.join("Skyrim.esm"), b"TES4").unwrap();
        assert_eq!(identify_skyrim_at(&root), Some(GameEdition::SE));
        let g = find_skyrim_at_smart(&root).expect("data-only install");
        assert_eq!(g.data_dir, data);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn accepts_data_folder_path() {
        let root = tmp();
        let data = root.join("Data");
        fs::create_dir_all(&data).unwrap();
        fs::write(data.join("Skyrim.esm"), b"TES4").unwrap();
        fs::write(root.join("SkyrimSE.exe"), b"x").unwrap();
        let g = find_skyrim_at_smart(&data).expect("from Data path");
        assert_eq!(g.install_dir, root);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn nested_scan_finds_install() {
        let root = tmp();
        let nested = root.join("Games").join("RPG").join("SkyrimSE");
        fs::create_dir_all(nested.join("Data")).unwrap();
        fs::write(nested.join("Data").join("Skyrim.esm"), b"TES4").unwrap();
        let found = scan_folder_for_skyrim(&root, 6);
        assert!(
            found.iter().any(|d| d.game.install_dir == nested),
            "expected nested install, got {:?}",
            found.iter().map(|d| d.game.install_dir.clone()).collect::<Vec<_>>()
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn derive_id_stable_for_same_path() {
        let p = PathBuf::from("/tmp/some/skyrim/install");
        assert_eq!(derive_id(&p), derive_id(&p));
    }
}
