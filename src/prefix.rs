//! Locate Wine/Proton prefixes on Linux and figure out, inside each one,
//! which `drive_c/users/<name>` folder is the one actually in use — since
//! Proton always uses `steamuser`, while Lutris/bottles/custom `WINEPREFIX`
//! setups typically use the real Linux username (or whatever `$USER` was
//! when the prefix was created).

use anyhow::Result;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct WinePrefix {
    /// Root of the prefix, i.e. the directory containing `drive_c`.
    pub prefix_root: PathBuf,
    /// The active Windows user folder inside `drive_c/users/`, e.g.
    /// `drive_c/users/steamuser` or `drive_c/users/plation`.
    pub windows_user_dir: PathBuf,
    /// Where this prefix came from, for display purposes.
    pub source: PrefixSource,
}

#[derive(Debug, Clone)]
pub enum PrefixSource {
    /// A Steam/Proton compatdata prefix. `game_name` is resolved from the
    /// Steam library's `appmanifest_<appid>.acf` when available, so the
    /// disambiguation UI can show "Skyrim Special Edition (Steam, Proton)"
    /// rather than just a bare app id.
    SteamProton {
        app_id: String,
        game_name: Option<String>,
    },
    Lutris { name: String },
    Heroic { name: String },
    Bottles { name: String },
    PortProton { name: String },
    CrossOver { name: String },
    /// Plain `~/.wine` (Wine's own default prefix, shared across everything
    /// run without an explicit `WINEPREFIX`).
    PlainWine,
    Custom { path: PathBuf },
}

impl PrefixSource {
    /// One-line human label for disambiguation prompts, e.g.
    /// "Steam/Proton — Skyrim Special Edition (appid 489830)".
    pub fn label(&self) -> String {
        match self {
            PrefixSource::SteamProton { app_id, game_name } => match game_name {
                Some(name) => format!("Steam/Proton — {name} (appid {app_id})"),
                None => format!("Steam/Proton — appid {app_id}"),
            },
            PrefixSource::Lutris { name } => format!("Lutris — {name}"),
            PrefixSource::Heroic { name } => format!("Heroic Games Launcher — {name}"),
            PrefixSource::Bottles { name } => format!("Bottles — {name}"),
            PrefixSource::PortProton { name } => format!("PortProton — {name}"),
            PrefixSource::CrossOver { name } => format!("CrossOver — {name}"),
            PrefixSource::PlainWine => "Plain Wine (~/.wine)".to_string(),
            PrefixSource::Custom { path } => format!("Custom prefix — {}", path.display()),
        }
    }
}

impl WinePrefix {
    /// Absolute path to `drive_c` within this prefix.
    pub fn drive_c(&self) -> PathBuf {
        self.prefix_root.join("drive_c")
    }

    /// `drive_c/users/<user>/Documents/My Games` — where Skyrim's INI files
    /// and (for some mods) saves live.
    pub fn my_games_dir(&self) -> PathBuf {
        self.windows_user_dir.join("Documents").join("My Games")
    }
}

/// Find the correct `drive_c/users/*` directory inside a prefix without
/// assuming a name. Preference order:
///   1. A folder that already has a `Documents/My Games/Skyrim*` subfolder
///      (proof it's been used to run Skyrim before).
///   2. `steamuser` if present (Proton's default, most common case).
///   3. The real Linux username, if a matching folder exists.
///   4. Whatever single non-"Public"/"Default" folder exists.
pub fn resolve_windows_user_dir(prefix_root: &Path) -> Result<PathBuf> {
    let users_dir = prefix_root.join("drive_c").join("users");
    let mut candidates = Vec::new();
    if users_dir.is_dir() {
        for entry in std::fs::read_dir(&users_dir)? {
            let entry = entry?;
            if entry.path().is_dir() {
                candidates.push(entry.path());
            }
        }
    }

    // 1. Already has Skyrim save/ini data.
    for c in &candidates {
        let my_games = c.join("Documents").join("My Games");
        if my_games.is_dir() {
            if let Ok(sub) = std::fs::read_dir(&my_games) {
                for entry in sub.flatten() {
                    let name = entry.file_name().to_string_lossy().to_lowercase();
                    if name.starts_with("skyrim") {
                        return Ok(c.clone());
                    }
                }
            }
        }
    }

    // 2. steamuser (Proton default).
    if let Some(c) = candidates
        .iter()
        .find(|c| c.file_name().map(|n| n == "steamuser").unwrap_or(false))
    {
        return Ok(c.clone());
    }

    // 3. Real Linux username.
    if let Some(linux_user) = std::env::var_os("USER").map(|s| s.to_string_lossy().to_string()) {
        if let Some(c) = candidates
            .iter()
            .find(|c| c.file_name().map(|n| n.to_string_lossy() == linux_user).unwrap_or(false))
        {
            return Ok(c.clone());
        }
    }

    // 4. First folder that isn't Public/Default/"All Users".
    if let Some(c) = candidates.iter().find(|c| {
        let name = c.file_name().unwrap_or_default().to_string_lossy().to_lowercase();
        !matches!(name.as_str(), "public" | "default" | "all users")
    }) {
        return Ok(c.clone());
    }

    anyhow::bail!(
        "no usable Windows user directory found under {}",
        users_dir.display()
    )
}

/// Scan the usual locations for Wine/Proton prefixes that might contain a
/// Skyrim install. Returns every prefix found; callers should further check
/// `game::find_skyrim_in_prefix` to confirm one actually has the game.
pub fn discover_prefixes() -> Vec<WinePrefix> {
    let mut found = Vec::new();
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return found,
    };

    // --- Steam / Proton compatdata ---
    // Covers native Steam, Steam Deck (same layout as native), flatpak
    // Steam, and snap Steam, plus any additional library folders referenced
    // from libraryfolders.vdf (e.g. a game installed on a second drive).
    let steam_roots = [
        home.join(".steam/steam"),
        home.join(".local/share/Steam"),
        home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam"), // flatpak Steam
        home.join("snap/steam/common/.local/share/Steam"),                // snap Steam
    ];
    for steam_root in &steam_roots {
        collect_steam_compatdata(steam_root, &mut found);
        for lib in additional_steam_libraries(steam_root) {
            collect_steam_compatdata(&lib, &mut found);
        }
    }

    // --- Lutris --- (prefixes commonly at <game>/prefix, or the game
    // install dir itself containing drive_c directly)
    collect_simple_prefixes(
        &[home.join("Games"), home.join(".local/share/lutris/runners/wine")],
        &[Some("prefix")],
        &mut found,
        |name| PrefixSource::Lutris { name },
    );

    // --- Heroic Games Launcher --- (Epic/GOG games run via GE-Proton/Wine;
    // default prefix root is ~/Games/Heroic/Prefixes/<AppName>, though it's
    // user-configurable per-game in Heroic's settings)
    collect_simple_prefixes(
        &[home.join("Games/Heroic/Prefixes"), home.join(".config/heroic/Prefixes")],
        &[None],
        &mut found,
        |name| PrefixSource::Heroic { name },
    );

    // --- Bottles --- (native + flatpak; each "bottle" is its own prefix)
    collect_simple_prefixes(
        &[
            home.join(".local/share/bottles/bottles"),
            home.join(".var/app/com.usebottles.bottles/data/bottles/bottles"),
        ],
        &[None],
        &mut found,
        |name| PrefixSource::Bottles { name },
    );

    // --- PortProton --- (native + flatpak; prefixes live under
    // data/prefixes/<name>, e.g. the default one is literally named DEFAULT)
    collect_simple_prefixes(
        &[
            home.join(".local/share/PortProton/data/prefixes"),
            home.join(".var/app/ru.linux_gaming.PortProton/data/prefixes"),
            home.join(".PortProton/data/prefixes"),
        ],
        &[None],
        &mut found,
        |name| PrefixSource::PortProton { name },
    );

    // --- CrossOver --- (~/.cxoffice/<bottle-name>)
    collect_simple_prefixes(&[home.join(".cxoffice")], &[None], &mut found, |name| {
        PrefixSource::CrossOver { name }
    });

    // --- Plain Wine default prefix ---
    let plain_wine = home.join(".wine");
    if plain_wine.join("drive_c").is_dir() {
        if let Ok(user_dir) = resolve_windows_user_dir(&plain_wine) {
            found.push(WinePrefix {
                prefix_root: plain_wine,
                windows_user_dir: user_dir,
                source: PrefixSource::PlainWine,
            });
        }
    }

    // --- Explicit WINEPREFIX env var, if set ---
    if let Some(wp) = std::env::var_os("WINEPREFIX") {
        let path = PathBuf::from(wp);
        if path.join("drive_c").is_dir() {
            if let Ok(user_dir) = resolve_windows_user_dir(&path) {
                found.push(WinePrefix {
                    prefix_root: path.clone(),
                    windows_user_dir: user_dir,
                    source: PrefixSource::Custom { path },
                });
            }
        }
    }

    found
}

/// Shared helper for launchers that lay out prefixes as
/// `<root>/<name>[/<subpath>]/drive_c`. `subpaths` lists optional subpaths
/// to also try under each named entry (e.g. Lutris' `.../<game>/prefix`);
/// pass `&[None]` to only check the entry itself.
fn collect_simple_prefixes(
    roots: &[PathBuf],
    subpaths: &[Option<&str>],
    found: &mut Vec<WinePrefix>,
    make_source: impl Fn(String) -> PrefixSource,
) {
    for root in roots {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let base = entry.path();
            for sub in subpaths {
                let candidate = match sub {
                    Some(s) => base.join(s),
                    None => base.clone(),
                };
                if candidate.join("drive_c").is_dir() {
                    if let Ok(user_dir) = resolve_windows_user_dir(&candidate) {
                        found.push(WinePrefix {
                            prefix_root: candidate,
                            windows_user_dir: user_dir,
                            source: make_source(name.clone()),
                        });
                    }
                }
            }
        }
    }
}

fn collect_steam_compatdata(steam_root: &Path, found: &mut Vec<WinePrefix>) {
    let compatdata = steam_root.join("steamapps").join("compatdata");
    let Ok(entries) = std::fs::read_dir(&compatdata) else {
        return;
    };
    for entry in entries.flatten() {
        let app_id = entry.file_name().to_string_lossy().to_string();
        let pfx = entry.path().join("pfx");
        if pfx.join("drive_c").is_dir() {
            if let Ok(user_dir) = resolve_windows_user_dir(&pfx) {
                let game_name = read_appmanifest_name(steam_root, &app_id);
                found.push(WinePrefix {
                    prefix_root: pfx,
                    windows_user_dir: user_dir,
                    source: PrefixSource::SteamProton { app_id, game_name },
                });
            }
        }
    }
}

/// Read the `"name"` field out of `steamapps/appmanifest_<appid>.acf`, Steam's
/// per-game metadata file, so prefixes can be labeled with a real game name
/// (e.g. "Skyrim Special Edition") instead of just an app id.
fn read_appmanifest_name(steam_root: &Path, app_id: &str) -> Option<String> {
    let acf_path = steam_root
        .join("steamapps")
        .join(format!("appmanifest_{app_id}.acf"));
    let contents = std::fs::read_to_string(acf_path).ok()?;
    for line in contents.lines() {
        let line = line.trim();
        if line.starts_with("\"name\"") {
            let parts: Vec<&str> = line.split('"').filter(|p| !p.trim().is_empty()).collect();
            return parts.last().map(|s| s.to_string());
        }
    }
    None
}

/// Parse `steamapps/libraryfolders.vdf` for additional Steam library roots
/// (e.g. games installed on a second drive), returning them as Steam roots
/// (i.e. paths that themselves contain a `steamapps` folder).
fn additional_steam_libraries(steam_root: &Path) -> Vec<PathBuf> {
    let vdf_path = steam_root.join("steamapps").join("libraryfolders.vdf");
    let mut libs = Vec::new();
    if let Ok(contents) = std::fs::read_to_string(&vdf_path) {
        for line in contents.lines() {
            let line = line.trim();
            if line.starts_with("\"path\"") {
                // crude VDF value extraction: `"path"		"/mnt/games/SteamLibrary"`
                // split on quotes and take the last non-empty token.
                let parts: Vec<&str> = line.split('"').filter(|p| !p.trim().is_empty()).collect();
                if let Some(path) = parts.last() {
                    libs.push(PathBuf::from(path));
                }
            }
        }
    }
    libs
}
