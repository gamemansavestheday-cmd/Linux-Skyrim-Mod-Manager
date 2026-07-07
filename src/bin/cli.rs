use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use std::io::Write;
use std::path::PathBuf;

use skyrim_modmgr::{
    app_paths::AppPaths,
    config::Config,
    game::{find_skyrim_at, scan_all_prefixes_for_skyrim, GameInstall},
    ini,
    profile::Profile,
    store::ModStore,
    validate, vfs,
};

#[derive(Parser)]
#[command(name = "skyrim-modmgr", version, about = "Cross-platform Skyrim mod manager")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Scan every Wine/Proton/PortProton/Lutris/Bottles/Heroic/CrossOver
    /// prefix for a Skyrim install. If more than one is found, you'll be
    /// asked which one to remember as "the" game. Pass --path to skip
    /// scanning and point directly at an install (used on native Windows).
    DetectGame {
        #[arg(long)]
        path: Option<PathBuf>,
        #[arg(long)]
        my_games_dir: Option<PathBuf>,
    },
    /// List every game install this app currently remembers, and which one
    /// is active.
    ListGames,
    /// Switch the active game to a previously detected install (by id
    /// prefix, see `list-games`).
    UseGame { id: String },

    /// Install a mod from a folder, .zip, .7z, .tar/.tar.gz, or a single
    /// loose file (a lone .esp, a standalone texture, anything) into the
    /// global mod store.
    Install {
        source: PathBuf,
        #[arg(long)]
        name: Option<String>,
        /// Comma-separated tags, e.g. --tags textures,armor
        #[arg(long)]
        tags: Option<String>,
    },
    /// Replace a mod's files in place from a new source, keeping its id (so
    /// every profile referencing it keeps working).
    Update { mod_id: String, source: PathBuf },
    /// Remove a mod from the store (also un-references it from every
    /// profile).
    Remove { mod_id: String },
    /// List all mods in the global store, optionally filtered by tag.
    ListMods {
        #[arg(long)]
        tag: Option<String>,
    },
    /// Add a tag to an installed mod.
    Tag { mod_id: String, tag: String },
    /// Show disk space used per mod, largest first, plus the total.
    DiskUsage,

    /// Create a new empty profile for the active game.
    NewProfile { name: String },
    /// List all profiles.
    ListProfiles,
    /// Duplicate a profile under a new name.
    CloneProfile { source: String, new_name: String },
    /// Rename a profile.
    RenameProfile { name: String, new_name: String },
    /// Delete a profile (does not touch the mod store).
    DeleteProfile { name: String },
    /// Enable a mod in a profile. Also auto-registers any plugins (.esp/
    /// .esm/.esl) it ships into the plugin load order.
    Enable { profile: String, mod_id: String },
    /// Disable a mod in a profile.
    Disable { profile: String, mod_id: String },
    /// Move a mod to a new position (0 = lowest priority) in a profile's
    /// load order.
    Reorder {
        profile: String,
        mod_id: String,
        index: usize,
    },
    /// Print a human-readable summary of a profile's load order (for
    /// sharing e.g. in Discord).
    ExportProfile { profile: String },
    /// Replace a profile's plugin load order from an existing plugins.txt
    /// (e.g. exported by MO2/Vortex).
    ImportPlugins { profile: String, plugins_txt: PathBuf },

    /// Show every file more than one enabled mod in a profile provides,
    /// and which one wins.
    Conflicts { profile: String },
    /// Check every enabled plugin in a profile for missing master files.
    Validate { profile: String },
    /// Preview a deploy (file count + conflicts) without touching disk.
    DryRun { profile: String },
    /// Deploy a profile: resolve conflicts, build the VFS, mount over Data.
    /// Uses the active game unless --install-dir/--my-games-dir are given.
    Deploy {
        profile: String,
        #[arg(long)]
        install_dir: Option<PathBuf>,
        #[arg(long)]
        my_games_dir: Option<PathBuf>,
    },
    /// Restore the active (or specified) game's Data folder to vanilla,
    /// undoing the current deploy.
    Restore {
        #[arg(long)]
        install_dir: Option<PathBuf>,
        #[arg(long)]
        my_games_dir: Option<PathBuf>,
    },

    /// Read or write a single Skyrim.ini/SkyrimPrefs.ini tweak.
    SetIni {
        ini_file: PathBuf,
        section: String,
        key: String,
        value: String,
    },
    GetIni {
        ini_file: PathBuf,
        section: String,
        key: String,
    },
}

fn prompt_pick(count: usize) -> Result<usize> {
    print!("Pick one [1-{count}]: ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let choice: usize = line.trim().parse().context("not a number")?;
    if choice == 0 || choice > count {
        bail!("choice out of range");
    }
    Ok(choice - 1)
}

fn require_game(config: &Config) -> Result<&GameInstall> {
    config
        .active_game()
        .context("no active game — run detect-game first")
}

fn main() -> Result<()> {
    // Rust's default SIGPIPE handling turns a broken pipe (e.g. this
    // program's output piped into `head` or `less`, which close the pipe
    // early) into a panic on the next print. That's surprising for a CLI
    // tool — piping output around is completely normal usage — so we
    // install a panic hook that recognizes this specific case and exits
    // quietly (code 0) instead of dumping a backtrace.
    std::panic::set_hook(Box::new(|info| {
        let msg = info.to_string();
        if msg.contains("Broken pipe") {
            std::process::exit(0);
        }
        eprintln!("{msg}");
    }));

    let cli = Cli::parse();
    let paths = AppPaths::discover().context("setting up app data directories")?;
    let mut config = Config::load(&paths)?;

    match cli.command {
        Commands::DetectGame { path, my_games_dir } => {
            if let Some(path) = path {
                let my_games = my_games_dir.clone().unwrap_or_else(|| path.clone());
                match find_skyrim_at(&path, &my_games) {
                    Some(g) => {
                        println!("Found: {:?} at {}", g.edition, g.install_dir.display());
                        config.remember_game(g);
                        config.save(&paths)?;
                    }
                    None => println!("No Skyrim install found at {}", path.display()),
                }
            } else {
                let mut found = scan_all_prefixes_for_skyrim();
                if found.is_empty() {
                    println!(
                        "No Skyrim install found in any Wine/Proton/PortProton/Lutris/Bottles/\
                         Heroic/CrossOver prefix. If it's installed somewhere unusual, use \
                         --path to point at it directly."
                    );
                    return Ok(());
                }

                let chosen = if found.len() == 1 {
                    found.remove(0)
                } else {
                    println!("Found {} Skyrim installs. Which one do you use?\n", found.len());
                    for (i, d) in found.iter().enumerate() {
                        let last_played = d
                            .data_dir_modified_secs
                            .map(|secs| {
                                let now = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_secs())
                                    .unwrap_or(0);
                                let days = now.saturating_sub(secs) / 86400;
                                if days == 0 {
                                    "Data folder touched today".to_string()
                                } else {
                                    format!("Data folder last touched {days} day(s) ago")
                                }
                            })
                            .unwrap_or_else(|| "no timestamp available".to_string());
                        println!(
                            "  [{}] {:?} — {}\n        {}\n        {}",
                            i + 1,
                            d.game.edition,
                            d.source_label,
                            d.game.install_dir.display(),
                            last_played
                        );
                    }
                    println!(
                        "\nTip: the one with the most recently touched Data folder is usually \
                         the one you actually play on — a fresh/never-modded prefix won't have \
                         been written to."
                    );
                    let idx = prompt_pick(found.len())?;
                    found.remove(idx)
                };

                println!(
                    "Using {:?} at {} ({})",
                    chosen.game.edition,
                    chosen.game.install_dir.display(),
                    chosen.source_label
                );
                config.remember_game(chosen.game);
                config.save(&paths)?;
            }
        }

        Commands::ListGames => {
            if config.known_games.is_empty() {
                println!("No games remembered yet — run detect-game.");
            }
            for g in &config.known_games {
                let active = if Some(&g.id) == config.active_game_id.as_ref() {
                    " (active)"
                } else {
                    ""
                };
                println!("{}  {:?}  {}{}", g.id, g.edition, g.install_dir.display(), active);
            }
        }

        Commands::UseGame { id } => {
            let found = config
                .known_games
                .iter()
                .find(|g| g.id.starts_with(&id))
                .context("no known game with that id prefix — see list-games")?;
            config.active_game_id = Some(found.id.clone());
            config.save(&paths)?;
            println!("Active game set to {}", found.install_dir.display());
        }

        Commands::Install { source, name, tags } => {
            let mut store = ModStore::load(&paths)?;
            let id = store.install(&paths, &source, name)?;
            if let Some(tags) = tags {
                for tag in tags.split(',').map(|t| t.trim()).filter(|t| !t.is_empty()) {
                    store.add_tag(&paths, &id, tag)?;
                }
            }
            println!("Installed as mod id: {id}");
        }

        Commands::Update { mod_id, source } => {
            let mut store = ModStore::load(&paths)?;
            store.update(&paths, &mod_id, &source)?;
            println!("Updated {mod_id} from {}", source.display());
        }

        Commands::Remove { mod_id } => {
            let mut store = ModStore::load(&paths)?;
            store.remove(&paths, &mod_id)?;
            println!("Removed {mod_id}");
        }

        Commands::ListMods { tag } => {
            let store = ModStore::load(&paths)?;
            let mods: Vec<_> = match &tag {
                Some(tag) => store.mods_with_tag(tag).collect(),
                None => store.mods.iter().collect(),
            };
            for m in mods {
                let tags = if m.tags.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", m.tags.join(", "))
                };
                println!("{}  {}{}  ({})", m.id, m.name, tags, m.content_dir.display());
            }
        }

        Commands::Tag { mod_id, tag } => {
            let mut store = ModStore::load(&paths)?;
            store.add_tag(&paths, &mod_id, &tag)?;
            println!("Tagged {mod_id} with '{tag}'");
        }

        Commands::DiskUsage => {
            let store = ModStore::load(&paths)?;
            let (per_mod, total) = store.disk_usage();
            for (id, name, size) in &per_mod {
                println!("{:>10}  {}  ({})", human_size(*size), name, id);
            }
            println!("---\nTotal: {}", human_size(total));
        }

        Commands::NewProfile { name } => {
            let game = require_game(&config)?;
            let profile = Profile::new(&name, game.id.clone());
            profile.save(&paths)?;
            println!("Created profile '{name}'");
        }

        Commands::ListProfiles => {
            for name in Profile::list_all(&paths)? {
                println!("{name}");
            }
        }

        Commands::CloneProfile { source, new_name } => {
            let p = Profile::load(&paths, &source)?;
            p.clone_as(&paths, &new_name)?;
            println!("Cloned '{source}' -> '{new_name}'");
        }

        Commands::RenameProfile { name, new_name } => {
            let mut p = Profile::load(&paths, &name)?;
            p.rename(&paths, &new_name)?;
            println!("Renamed '{name}' -> '{new_name}'");
        }

        Commands::DeleteProfile { name } => {
            Profile::delete(&paths, &name)?;
            println!("Deleted profile '{name}'");
        }

        Commands::Enable { profile, mod_id } => {
            let store = ModStore::load(&paths)?;
            let mut p = Profile::load(&paths, &profile)?;
            p.enable_mod_with_plugins(&store, &mod_id);
            p.save(&paths)?;
            println!("Enabled {mod_id} in profile '{profile}' (plugins auto-registered if any)");
        }

        Commands::Disable { profile, mod_id } => {
            let mut p = Profile::load(&paths, &profile)?;
            p.disable_mod(&mod_id);
            p.save(&paths)?;
            println!("Disabled {mod_id} in profile '{profile}'");
        }

        Commands::Reorder { profile, mod_id, index } => {
            let mut p = Profile::load(&paths, &profile)?;
            p.reorder(&mod_id, index);
            p.save(&paths)?;
            println!("Reordered {mod_id} to position {index} in '{profile}'");
        }

        Commands::ExportProfile { profile } => {
            let store = ModStore::load(&paths)?;
            let p = Profile::load(&paths, &profile)?;
            println!("{}", p.export_readable(&store));
        }

        Commands::ImportPlugins { profile, plugins_txt } => {
            let mut p = Profile::load(&paths, &profile)?;
            p.import_plugins_txt(&plugins_txt)?;
            p.save(&paths)?;
            println!("Imported plugin order from {}", plugins_txt.display());
        }

        Commands::Conflicts { profile } => {
            let store = ModStore::load(&paths)?;
            let p = Profile::load(&paths, &profile)?;
            let detailed = vfs::resolve_conflicts_detailed(&store, &p)?;
            let mut any = false;
            for (path, contributions) in &detailed {
                if contributions.len() > 1 {
                    any = true;
                    let winner = &contributions.last().unwrap().mod_id;
                    println!("{path}");
                    for c in contributions {
                        let name = store.get(&c.mod_id).map(|m| m.name.as_str()).unwrap_or(&c.mod_id);
                        let marker = if &c.mod_id == winner { "WINS" } else { "    " };
                        println!("    [{marker}] {name}");
                    }
                }
            }
            if !any {
                println!("No file conflicts between enabled mods in '{profile}'.");
            }
        }

        Commands::Validate { profile } => {
            let store = ModStore::load(&paths)?;
            let p = Profile::load(&paths, &profile)?;
            // Build a lookup from plugin filename -> path on disk by
            // scanning every enabled mod's content dir once.
            let mut plugin_paths = std::collections::HashMap::new();
            for mod_id in p.enabled_mods_in_order() {
                if let Some(entry) = store.get(mod_id) {
                    if let Ok(read_dir) = std::fs::read_dir(&entry.content_dir) {
                        for e in read_dir.flatten() {
                            let name = e.file_name().to_string_lossy().to_string();
                            plugin_paths.insert(name.to_lowercase(), e.path());
                        }
                    }
                }
            }
            let problems = validate::check_missing_masters(&p.plugin_order, |plugin| {
                plugin_paths.get(&plugin.to_lowercase()).cloned()
            });
            if problems.is_empty() {
                println!("No missing masters detected for '{profile}'.");
            } else {
                for problem in problems {
                    println!("{} is missing master(s):", problem.plugin);
                    for m in problem.missing {
                        println!("    {m}");
                    }
                }
            }
        }

        Commands::DryRun { profile } => {
            let store = ModStore::load(&paths)?;
            let p = Profile::load(&paths, &profile)?;
            let report = vfs::deploy_dry_run(&store, &p)?;
            println!("Would deploy {} files.", report.total_files);
            if report.conflicts.is_empty() {
                println!("No conflicts.");
            } else {
                println!("{} file(s) have conflicts (last listed wins):", report.conflicts.len());
                for c in &report.conflicts {
                    let path = &c.last().unwrap().relative_path;
                    println!("  {}", path.display());
                }
            }
        }

        Commands::Deploy { profile, install_dir, my_games_dir } => {
            let store = ModStore::load(&paths)?;
            let p = Profile::load(&paths, &profile)?;
            let game = resolve_game(&config, install_dir, my_games_dir)?;
            let backend = vfs::PlatformBackend;
            let count = vfs::deploy(&paths, &backend, &store, &p, &game)?;
            println!("Deployed {count} files for profile '{profile}'");
        }

        Commands::Restore { install_dir, my_games_dir } => {
            let game = resolve_game(&config, install_dir, my_games_dir)?;
            let backend = vfs::PlatformBackend;
            vfs::restore(&paths, &backend, &game)?;
            println!("Restored vanilla Data folder for {}", game.install_dir.display());
        }

        Commands::SetIni { ini_file, section, key, value } => {
            ini::set_ini_value(&ini_file, &section, &key, &value)?;
            println!("Set [{section}] {key}={value} in {}", ini_file.display());
        }

        Commands::GetIni { ini_file, section, key } => {
            match ini::get_ini_value(&ini_file, &section, &key) {
                Some(v) => println!("{v}"),
                None => println!("(not set)"),
            }
        }
    }

    Ok(())
}

/// Resolve which `GameInstall` to act on: explicit --install-dir wins, else
/// fall back to the config's active game.
fn resolve_game(
    config: &Config,
    install_dir: Option<PathBuf>,
    my_games_dir: Option<PathBuf>,
) -> Result<GameInstall> {
    if let Some(install_dir) = install_dir {
        let my_games = my_games_dir.unwrap_or_else(|| install_dir.clone());
        return find_skyrim_at(&install_dir, &my_games)
            .context("could not confirm a Skyrim install at the given path");
    }
    require_game(config).cloned()
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[0])
    } else {
        format!("{:.1} {}", size, UNITS[unit])
    }
}
