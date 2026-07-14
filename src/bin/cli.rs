use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use std::io::Write;
use std::path::PathBuf;

use skyrim_modmgr::{
    app_paths::AppPaths,
    bsa, check,
    color,
    config::Config,
    fomod,
    game::{find_skyrim_at, scan_all_locations, GameInstall},
    ini, nxm,
    profile::Profile,
    store::{self, ModStore},
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
    /// Add a folder to search for a Skyrim install, alongside the default
    /// Wine/Proton prefix scan and the Downloads/Games/Desktop folders —
    /// for a copy that lives somewhere non-standard (a second drive, a
    /// portable install, etc). Persisted; checked on every `detect-game`.
    AddSearchPath { path: PathBuf },
    /// Remove a previously added custom search path.
    RemoveSearchPath { path: PathBuf },
    /// List custom search paths currently configured.
    ListSearchPaths,

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
        /// Only estimate size / warn about disk space; do not install.
        #[arg(long)]
        dry_run: bool,
    },
    /// Estimate extracted size of a mod source without installing it.
    EstimateSize { source: PathBuf },
    /// Install a mod that uses a FOMOD installer (fomod/ModuleConfig.xml) —
    /// walks you through the same step-by-step choices MO2/Vortex show,
    /// then installs only the files your choices resolve to.
    InstallFomod {
        source: PathBuf,
        #[arg(long)]
        name: Option<String>,
    },
    /// Replace a mod's files in place from a new source, keeping its id (so
    /// every profile referencing it keeps working).
    Update { mod_id: String, source: PathBuf },
    /// Remove a mod from the store (also un-references it from every
    /// profile). Prompts for confirmation unless --yes is given.
    Remove {
        mod_id: String,
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// List all mods in the global store, optionally filtered by tag,
    /// searched by name/tag substring, and sorted.
    ListMods {
        #[arg(long)]
        tag: Option<String>,
        /// Case-insensitive substring match against name or tags.
        #[arg(long)]
        search: Option<String>,
        /// Sort order: name | date | size (defaults to install order).
        #[arg(long)]
        sort: Option<String>,
    },
    /// Add a tag to an installed mod.
    Tag { mod_id: String, tag: String },
    /// List every distinct tag currently in use across the store.
    ListTags,
    /// Print a mod's content directory path, and try to open it in the
    /// system file manager (best-effort — falls back to just printing the
    /// path if no opener is available).
    ModPath {
        mod_id: String,
        #[arg(long)]
        open: bool,
    },
    /// Show disk space used per mod, largest first, plus the total.
    DiskUsage,
    /// Search which installed mod(s) provide a given relative file path
    /// (case-insensitive substring match).
    WhichMod { path: String },

    /// Create a new empty profile for the active game.
    NewProfile { name: String },
    /// Switch the active profile (used as the default wherever a
    /// `<profile>` argument is omitted below).
    UseProfile { name: String },
    /// List all profiles.
    ListProfiles,
    /// Duplicate a profile under a new name.
    CloneProfile { source: String, new_name: String },
    /// Rename a profile.
    RenameProfile { name: String, new_name: String },
    /// Delete a profile (does not touch the mod store). Prompts for
    /// confirmation unless --yes is given.
    DeleteProfile {
        name: String,
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// Enable a mod in a profile (defaults to the active profile). Also
    /// auto-registers any plugins (.esp/.esm/.esl) it ships into the
    /// plugin load order.
    Enable {
        mod_id: String,
        #[arg(long)]
        profile: Option<String>,
    },
    /// Disable a mod in a profile (defaults to the active profile).
    Disable {
        mod_id: String,
        #[arg(long)]
        profile: Option<String>,
    },
    /// Enable several mods at once (comma-separated ids).
    EnableMany {
        /// Comma-separated mod ids
        mod_ids: String,
        #[arg(long)]
        profile: Option<String>,
    },
    /// Disable several mods at once (comma-separated ids).
    DisableMany {
        mod_ids: String,
        #[arg(long)]
        profile: Option<String>,
    },
    /// Enable every mod in the store in a profile.
    EnableAll {
        #[arg(long)]
        profile: Option<String>,
    },
    /// Disable every mod in a profile.
    DisableAll {
        #[arg(long)]
        profile: Option<String>,
    },
    /// Move a mod to a new position (0 = lowest priority) in a profile's
    /// load order.
    Reorder {
        mod_id: String,
        index: usize,
        #[arg(long)]
        profile: Option<String>,
    },
    /// Print a human-readable summary of a profile's load order (for
    /// sharing e.g. in Discord).
    ExportProfile {
        #[arg(long)]
        profile: Option<String>,
    },
    /// Replace a profile's plugin load order from an existing plugins.txt
    /// (e.g. exported by MO2/Vortex).
    ImportPlugins {
        plugins_txt: PathBuf,
        #[arg(long)]
        profile: Option<String>,
    },

    /// Show every file more than one enabled mod in a profile provides,
    /// and which one wins.
    Conflicts {
        #[arg(long)]
        profile: Option<String>,
    },
    /// Check every enabled plugin in a profile for missing master files.
    Validate {
        #[arg(long)]
        profile: Option<String>,
    },
    /// Check a profile's plugin load order for circular/impossible master
    /// dependencies without attempting to sort it.
    Cycles {
        #[arg(long)]
        profile: Option<String>,
    },
    /// Automatically sort a profile's plugin load order by master
    /// dependency (a plugin always ends up after every master it needs).
    /// Plugins with no dependency relationship keep their relative order.
    /// Refuses to write anything if a circular dependency is found.
    Sort {
        #[arg(long)]
        profile: Option<String>,
        /// Show the resulting order without saving it.
        #[arg(long)]
        dry_run: bool,
    },
    /// Preview a deploy (file count + conflicts) without touching disk.
    DryRun {
        #[arg(long)]
        profile: Option<String>,
    },
    /// Deploy a profile: resolve conflicts, build the VFS, mount over Data.
    /// Uses the active game and active profile unless overridden.
    Deploy {
        #[arg(long)]
        profile: Option<String>,
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

    /// Run the automated testing / diagnostics suite and print a single
    /// readable report (cargo test, clippy, fuzz, round-trip, audits…).
    Doctor {
        /// Also write a markdown report to this path.
        #[arg(long)]
        markdown: Option<PathBuf>,
    },

    /// Print a shell completion script to stdout (bash, zsh, or fish).
    Completions {
        /// Shell name: bash | zsh | fish
        shell: String,
    },

    /// Register this tool as the handler for Nexus Mods' "Mod Manager
    /// Download" button (writes a .desktop file and registers it via
    /// xdg-mime, best-effort). Linux only.
    RegisterNxmHandler {
        /// Path to the skyrim-modmgr binary to register (defaults to the
        /// currently running executable's path).
        #[arg(long)]
        binary_path: Option<PathBuf>,
    },
    /// Handle an nxm:// URL (this is what RegisterNxmHandler points
    /// "Mod Manager Download" at). Parses the link and tells you what was
    /// requested — this version does not yet download it automatically,
    /// see the roadmap for full Nexus API integration.
    HandleNxm { url: String },

    /// Launch the active game — prefers an SKSE loader if one is present
    /// in the install directory, falls back to the vanilla exe otherwise.
    Launch,
    /// Undo the most recent profile-mutating action (enable/disable/
    /// reorder/sort/tag-related changes to a profile's mod or plugin
    /// order). Single-step only — there's one undo slot, not a full
    /// history; a second `undo` undoes the undo (i.e. redoes).
    Undo,
    /// Remove leftover scratch directories from interrupted operations
    /// (e.g. a FOMOD install that didn't finish cleanly).
    CleanTmp,
}

/// Prompt for a y/n confirmation before a destructive action. Defaults to
/// "no" on anything other than an explicit y/yes, including a bare Enter —
/// a destructive-action prompt should never accidentally proceed on a
/// stray keypress.
fn confirm(message: &str) -> Result<bool> {
    print!("{message} [y/N]: ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(matches!(line.trim().to_lowercase().as_str(), "y" | "yes"))
}

fn prompt_pick(count: usize) -> Result<usize> {
    print!("Pick one [1-{count}]: ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let choice: usize = line
        .trim()
        .parse()
        .context("not a number — enter the index shown in brackets")?;
    if choice == 0 || choice > count {
        bail!("choice out of range (need 1–{count})");
    }
    Ok(choice - 1)
}

/// Prompt for a FOMOD option-group selection, respecting the group's type:
/// exactly-one / at-most-one get a single-pick prompt; any/all/at-least-one
/// get a comma-separated multi-pick prompt (SelectAll pre-fills and doesn't
/// actually need to ask, but we still confirm so the person sees what's
/// about to be installed).
fn prompt_group_choice(group: &fomod::OptionGroup) -> Result<Vec<usize>> {
    use skyrim_modmgr::fomod::GroupType;
    let n = group.options.len();
    if n == 0 {
        return Ok(Vec::new());
    }
    match group.group_type {
        GroupType::SelectAll => {
            println!("  (all options in this group are installed)");
            Ok((0..n).collect())
        }
        GroupType::SelectExactlyOne => {
            let idx = prompt_pick(n)?;
            Ok(vec![idx])
        }
        GroupType::SelectAtMostOne => {
            print!("  Pick one [1-{n}], or press Enter for none: ");
            std::io::stdout().flush().ok();
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return Ok(Vec::new());
            }
            let choice: usize = trimmed
                .parse()
                .context("not a number — enter the index shown in brackets, or leave blank")?;
            if choice == 0 || choice > n {
                bail!("choice out of range (need 1–{n})");
            }
            Ok(vec![choice - 1])
        }
        GroupType::SelectAny | GroupType::SelectAtLeastOne => {
            print!("  Pick any number, comma-separated [1-{n}]: ");
            std::io::stdout().flush().ok();
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                if group.group_type == GroupType::SelectAtLeastOne {
                    bail!("this group requires at least one selection");
                }
                return Ok(Vec::new());
            }
            let mut picks = Vec::new();
            for part in trimmed.split(',') {
                let choice: usize = part
                    .trim()
                    .parse()
                    .with_context(|| format!("'{part}' is not a number"))?;
                if choice == 0 || choice > n {
                    bail!("choice {choice} out of range (need 1–{n})");
                }
                picks.push(choice - 1);
            }
            Ok(picks)
        }
    }
}

/// Scan every known prefix type for a Skyrim install and, if more than one
/// is found, interactively ask which one to use. Returns None (with a
/// warning already printed) if nothing was found — that's a normal,
/// expected outcome, not an error, since "no game installed yet" is valid
/// state for e.g. a fresh checkout.
fn scan_and_pick_game(
    extra_search_paths: &[PathBuf],
) -> Result<Option<skyrim_modmgr::game::DetectedGame>> {
    let mut found = scan_all_locations(extra_search_paths);
    if found.is_empty() {
        color::warn(
            "No Skyrim install found in any Wine/Proton/PortProton/Lutris/Bottles/\
             Heroic/CrossOver prefix, or in Downloads/Games/Desktop, or in any \
             custom search path (see `add-search-path`). If it's installed \
             somewhere else, use --path to point at it directly.",
        );
        return Ok(None);
    }

    if found.len() == 1 {
        return Ok(Some(found.remove(0)));
    }

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
    Ok(Some(found.remove(idx)))
}

/// Get the active game, auto-running a prefix scan first if this looks
/// like a genuinely first-time setup (no games known at all yet) — saves
/// having to separately run `detect-game` before the very first
/// `new-profile`/`deploy`. Only triggers when `known_games` is empty, not
/// just when nothing is currently active — if games are known but nothing
/// is selected, that's a deliberate state (e.g. after `use-game` was never
/// run) and auto-picking one would be more surprising than helpful.
fn ensure_active_game(config: &mut Config, paths: &AppPaths) -> Result<GameInstall> {
    if let Some(g) = config.active_game() {
        return Ok(g.clone());
    }
    if config.known_games.is_empty() {
        color::info("No game set up yet — scanning for a Skyrim install…");
        if let Some(chosen) = scan_and_pick_game(&config.extra_search_paths)? {
            color::success(&format!(
                "Using {:?} at {} ({})",
                chosen.game.edition,
                chosen.game.install_dir.display(),
                chosen.source_label
            ));
            config.remember_game(chosen.game.clone());
            config.save(paths)?;
            return Ok(chosen.game);
        }
    }
    bail!(
        "no active game — run `skyrim-modmgr detect-game` first \
         (or `use-game <id>` after list-games)"
    )
}


fn friendly_err(err: anyhow::Error) -> anyhow::Error {
    // Attach a short suggested fix for a few common failure modes.
    let msg = format!("{err:#}");
    let hint = if msg.contains("No such file") || msg.contains("does not exist") {
        Some("Hint: check the path exists and you have permission to read it.")
    } else if msg.contains("Permission denied") {
        Some("Hint: check file ownership/permissions, or that the game isn't locking Data.")
    } else if msg.contains("no active game") {
        Some("Hint: run detect-game once and pick your install.")
    } else if msg.contains("corrupt") {
        Some("Hint: the broken file was quarantined with a .corrupt.* suffix; defaults were restored.")
    } else {
        None
    };
    match hint {
        Some(h) => err.context(h.to_string()),
        None => err,
    }
}

fn main() {
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

    if let Err(e) = run() {
        let e = friendly_err(e);
        color::error(&format!("{e:#}"));
        std::process::exit(1);
    }
}

/// Save the pre-mutation state of a profile as the single undo slot,
/// overwriting whatever was there before. Called right after loading a
/// profile and before mutating it, in every profile-mutating command.
fn write_undo_snapshot(paths: &AppPaths, profile: &Profile) -> Result<()> {
    let slot_path = paths.root.join("undo.json");
    std::fs::write(&slot_path, serde_json::to_string_pretty(profile)?)
        .with_context(|| format!("writing {}", slot_path.display()))
}

/// Resolve a `--profile` flag: use it if given, otherwise fall back to the
/// config's active profile, erroring with a clear suggestion if neither is
/// set (rather than a bare "None" panic or silent wrong-profile behavior).
fn resolve_profile(config: &Config, given: Option<String>) -> Result<String> {
    given.or_else(|| config.active_profile.clone()).context(
        "no profile given and no active profile set — pass --profile <name>, \
         or run `use-profile <name>` to set a default",
    )
}

/// Build a lowercase-filename -> path lookup by scanning every enabled
/// mod's content dir in a profile. Used anywhere we need to hand
/// `validate`'s functions a way to actually read a plugin's bytes off disk.
fn build_plugin_path_lookup(
    store: &ModStore,
    profile: &Profile,
) -> std::collections::HashMap<String, PathBuf> {
    let mut plugin_paths = std::collections::HashMap::new();
    for mod_id in profile.enabled_mods_in_order() {
        if let Some(entry) = store.get(mod_id) {
            if let Ok(read_dir) = std::fs::read_dir(&entry.content_dir) {
                for e in read_dir.flatten() {
                    let name = e.file_name().to_string_lossy().to_string();
                    plugin_paths.insert(name.to_lowercase(), e.path());
                }
            }
        }
    }
    plugin_paths
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let paths = AppPaths::discover().context(
        "setting up app data directories — check that your home/data dir is writable",
    )?;
    let mut config = Config::load(&paths)?;
    let cfg_problems = config.validate();
    if !cfg_problems.is_empty() {
        for p in &cfg_problems {
            color::warn(p);
        }
        config.repair_in_memory();
        let _ = config.save(&paths);
    }

    match cli.command {
        Commands::DetectGame { path, my_games_dir } => {
            if let Some(path) = path {
                let my_games = my_games_dir.clone().unwrap_or_else(|| path.clone());
                match find_skyrim_at(&path, &my_games) {
                    Some(g) => {
                        color::success(&format!(
                            "Found {:?} at {}",
                            g.edition,
                            g.install_dir.display()
                        ));
                        config.remember_game(g);
                        config.save(&paths)?;
                    }
                    None => color::warn(&format!(
                        "No Skyrim install found at {} — expected SkyrimSE.exe / TESV.exe / SkyrimVR.exe",
                        path.display()
                    )),
                }
            } else if let Some(chosen) = scan_and_pick_game(&config.extra_search_paths)? {
                color::success(&format!(
                    "Using {:?} at {} ({})",
                    chosen.game.edition,
                    chosen.game.install_dir.display(),
                    chosen.source_label
                ));
                config.remember_game(chosen.game);
                config.save(&paths)?;
            }
        }

        Commands::ListGames => {
            if config.known_games.is_empty() {
                color::info("No games remembered yet — run detect-game.");
            }
            for g in &config.known_games {
                let active = if Some(&g.id) == config.active_game_id.as_ref() {
                    color::green(" (active)")
                } else {
                    String::new()
                };
                println!(
                    "{}  {:?}  {}{}",
                    g.id,
                    g.edition,
                    g.install_dir.display(),
                    active
                );
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
            color::success(&format!(
                "Active game set to {}",
                found.install_dir.display()
            ));
        }

        Commands::AddSearchPath { path } => {
            let canonical = path.canonicalize().unwrap_or(path.clone());
            config.add_search_path(canonical.clone());
            config.save(&paths)?;
            color::success(&format!(
                "Added {} to custom search paths — it'll be checked on the next detect-game.",
                canonical.display()
            ));
        }

        Commands::RemoveSearchPath { path } => {
            let canonical = path.canonicalize().unwrap_or(path.clone());
            config.remove_search_path(&canonical);
            config.save(&paths)?;
            color::success(&format!("Removed {} from custom search paths.", canonical.display()));
        }

        Commands::ListSearchPaths => {
            if config.extra_search_paths.is_empty() {
                color::info("No custom search paths configured.");
            }
            for p in &config.extra_search_paths {
                println!("{}", p.display());
            }
        }

        Commands::Install {
            source,
            name,
            tags,
            dry_run,
        } => {
            let estimate = ModStore::estimate_install_size(&source)?;
            println!(
                "Source: {} ({})",
                source.display(),
                estimate.source_kind
            );
            println!(
                "Estimated size: {} ({} file(s))",
                human_size(estimate.bytes),
                if estimate.files == 0 {
                    "?".into()
                } else {
                    estimate.files.to_string()
                }
            );
            if let Some(free) = store::free_space_for(&paths.mods) {
                println!("Free space on mod store volume: {}", human_size(free));
                if estimate.bytes > 0 && free < estimate.bytes.saturating_mul(2) {
                    color::warn(&format!(
                        "Disk space looks tight (need ~{}, have {}). Install may fail.",
                        human_size(estimate.bytes),
                        human_size(free)
                    ));
                }
            }
            if dry_run {
                color::info("Dry-run only — nothing installed.");
                return Ok(());
            }

            let mut store = ModStore::load(&paths)?;
            let id = store.install_with_progress(
                &paths,
                &source,
                name,
                Some(&|msg| {
                    check::progress_line(msg);
                }),
            )?;
            check::progress_done();
            if let Some(tags) = tags {
                for tag in tags.split(',').map(|t| t.trim()).filter(|t| !t.is_empty()) {
                    store.add_tag(&paths, &id, tag)?;
                }
            }
            color::success(&format!("Installed as mod id: {id}"));
        }

        Commands::InstallFomod { source, name } => {
            if !source.exists() {
                bail!("source path does not exist: {}", source.display());
            }
            let scratch = paths.tmp.join(format!("fomod-extract-{}", std::process::id()));
            if scratch.exists() {
                std::fs::remove_dir_all(&scratch)?;
            }
            std::fs::create_dir_all(&scratch)?;
            color::info("Extracting…");
            store::extract_archive_to(&source, &scratch)
                .context("extracting FOMOD archive")?;

            let Some(config_path) = fomod::find_module_config(&scratch) else {
                let _ = std::fs::remove_dir_all(&scratch);
                bail!(
                    "no fomod/ModuleConfig.xml found in {} — this doesn't look like a FOMOD \
                     archive. Use `install` for a plain mod instead.",
                    source.display()
                );
            };
            let archive_root = config_path
                .parent()
                .and_then(|fomod_dir| fomod_dir.parent())
                .unwrap_or(&scratch)
                .to_path_buf();
            let xml = std::fs::read_to_string(&config_path)
                .with_context(|| format!("reading {}", config_path.display()))?;
            let cfg = fomod::parse_module_config(&xml)?;

            let display_name = name.unwrap_or_else(|| {
                if cfg.module_name.is_empty() {
                    source
                        .file_stem()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_else(|| "FOMOD Mod".to_string())
                } else {
                    cfg.module_name.clone()
                }
            });

            println!("Installing '{display_name}' — {} step(s) to configure.\n", cfg.steps.len());

            let mut choices: Vec<fomod::StepChoices> = Vec::with_capacity(cfg.steps.len());
            for step in &cfg.steps {
                println!("== {} ==", step.name);
                let mut step_choices: fomod::StepChoices = Vec::with_capacity(step.groups.len());
                for group in &step.groups {
                    println!("-- {} --", group.name);
                    for (i, option) in group.options.iter().enumerate() {
                        let desc = if option.description.is_empty() {
                            String::new()
                        } else {
                            format!(" — {}", option.description)
                        };
                        println!("  [{}] {}{}", i + 1, option.name, desc);
                    }
                    let selected = prompt_group_choice(group)?;
                    step_choices.push(selected);
                }
                choices.push(step_choices);
            }

            let plan = fomod::resolve_install_plan(&cfg, &choices)?;
            let (id, content_dir) = ModStore::begin_content_dir(&paths)?;
            let file_count = fomod::apply_install_plan(&archive_root, &content_dir, &plan)
                .with_context(|| "applying FOMOD install plan")?;

            let mut store = ModStore::load(&paths)?;
            store.register_installed(&paths, id.clone(), content_dir, display_name.clone())?;
            let _ = std::fs::remove_dir_all(&scratch);

            color::success(&format!(
                "Installed '{display_name}' as {id} ({file_count} file(s))"
            ));
        }

        Commands::EstimateSize { source } => {
            let estimate = ModStore::estimate_install_size(&source)?;
            println!("kind:  {}", estimate.source_kind);
            println!("bytes: {} ({})", estimate.bytes, human_size(estimate.bytes));
            if estimate.files > 0 {
                println!("files: {}", estimate.files);
            }
            if let Some(free) = store::free_space_for(&paths.mods) {
                println!("free:  {} on mod-store volume", human_size(free));
                if free < estimate.bytes {
                    color::warn("Estimated size exceeds free space.");
                }
            }
        }

        Commands::Update { mod_id, source } => {
            let mut store = ModStore::load(&paths)?;
            store.update(&paths, &mod_id, &source)?;
            color::success(&format!(
                "Updated {mod_id} from {}",
                source.display()
            ));
        }

        Commands::Remove { mod_id, yes } => {
            let store_peek = ModStore::load(&paths)?;
            let display_name =
                store_peek.get(&mod_id).map(|m| m.name.clone()).unwrap_or_else(|| mod_id.clone());
            if !yes && !confirm(&format!(
                "Remove '{display_name}' ({mod_id})? This deletes its files and un-references \
                 it from every profile. This cannot be undone."
            ))? {
                color::info("Cancelled.");
                return Ok(());
            }
            let mut store = store_peek;
            store.remove(&paths, &mod_id)?;
            color::success(&format!("Removed {mod_id}"));
        }

        Commands::ListMods { tag, search, sort } => {
            let store = ModStore::load(&paths)?;
            let mut mods: Vec<_> = match &tag {
                Some(tag) => store.mods_with_tag(tag).collect(),
                None => store.mods.iter().collect(),
            };
            if let Some(term) = &search {
                let term = term.to_lowercase();
                mods.retain(|m| {
                    m.name.to_lowercase().contains(&term)
                        || m.tags.iter().any(|t| t.to_lowercase().contains(&term))
                });
            }
            match sort.as_deref() {
                Some("name") => mods.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase())),
                Some("date") => mods.sort_by_key(|m| std::cmp::Reverse(m.installed_at)),
                Some("size") => {
                    let (sizes, _total) = store.disk_usage();
                    let size_of: std::collections::HashMap<&str, u64> =
                        sizes.iter().map(|(id, _, sz)| (id.as_str(), *sz)).collect();
                    mods.sort_by_key(|m| {
                        std::cmp::Reverse(size_of.get(m.id.as_str()).copied().unwrap_or(0))
                    });
                }
                Some(other) => bail!("unknown sort '{other}' — use name, date, or size"),
                None => {}
            }
            if mods.is_empty() {
                color::info("No mods installed.");
            }
            for m in mods {
                let tags = if m.tags.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", m.tags.join(", "))
                };
                println!(
                    "{}  {}{}  ({})",
                    m.id,
                    m.name,
                    tags,
                    m.content_dir.display()
                );
            }
        }

        Commands::Tag { mod_id, tag } => {
            let mut store = ModStore::load(&paths)?;
            store.add_tag(&paths, &mod_id, &tag)?;
            color::success(&format!("Tagged {mod_id} with '{tag}'"));
        }

        Commands::ListTags => {
            let store = ModStore::load(&paths)?;
            let tags = store.all_tags();
            if tags.is_empty() {
                color::info("No tags in use yet — tag a mod with `tag <mod-id> <tag>`.");
            } else {
                for tag in tags {
                    println!("{tag}");
                }
            }
        }

        Commands::DiskUsage => {
            let store = ModStore::load(&paths)?;
            let (per_mod, total) = store.disk_usage();
            for (id, name, size) in &per_mod {
                println!("{:>10}  {}  ({})", human_size(*size), name, id);
            }
            println!("---\nTotal: {}", human_size(total));
        }

        Commands::WhichMod { path } => {
            let store = ModStore::load(&paths)?;
            let hits = store.mods_providing_file(&path);
            if hits.is_empty() {
                color::warn(&format!("No installed mod provides a path matching '{path}'"));
            } else {
                for (id, name, rel) in hits {
                    println!("{}  {}  ({})", id, name, rel.display());
                }
            }
        }

        Commands::NewProfile { name } => {
            let game = ensure_active_game(&mut config, &paths)?;
            let profile = Profile::new(&name, game.id.clone());
            profile.save(&paths)?;
            config.active_profile = Some(name.clone());
            config.save(&paths)?;
            color::success(&format!("Created profile '{name}' and set it active"));
        }

        Commands::UseProfile { name } => {
            // Confirm it actually exists before committing to it as active
            // — a typo here would otherwise silently break every
            // profile-less command until noticed.
            Profile::load(&paths, &name)
                .with_context(|| format!("no such profile '{name}' — see list-profiles"))?;
            config.active_profile = Some(name.clone());
            config.save(&paths)?;
            color::success(&format!("Active profile set to '{name}'"));
        }

        Commands::ListProfiles => {
            let names = Profile::list_all(&paths)?;
            if names.is_empty() {
                color::info("No profiles yet — run new-profile <name>.");
            }
            for name in names {
                let marker = if Some(&name) == config.active_profile.as_ref() {
                    " (active)"
                } else {
                    ""
                };
                println!("{name}{marker}");
            }
        }

        Commands::CloneProfile { source, new_name } => {
            let p = Profile::load(&paths, &source)?;
            p.clone_as(&paths, &new_name)?;
            color::success(&format!("Cloned '{source}' -> '{new_name}'"));
        }

        Commands::RenameProfile { name, new_name } => {
            let mut p = Profile::load(&paths, &name)?;
            p.rename(&paths, &new_name)?;
            if config.active_profile.as_deref() == Some(name.as_str()) {
                config.active_profile = Some(new_name.clone());
                config.save(&paths)?;
            }
            color::success(&format!("Renamed '{name}' -> '{new_name}'"));
        }

        Commands::DeleteProfile { name, yes } => {
            if !yes && !confirm(&format!(
                "Delete profile '{name}'? This cannot be undone."
            ))? {
                color::info("Cancelled.");
                return Ok(());
            }
            Profile::delete(&paths, &name)?;
            if config.active_profile.as_deref() == Some(name.as_str()) {
                config.active_profile = None;
                config.save(&paths)?;
            }
            color::success(&format!("Deleted profile '{name}'"));
        }

        Commands::Enable { profile, mod_id } => {
            let profile = resolve_profile(&config, profile)?;
            let store = ModStore::load(&paths)?;
            let mut p = Profile::load(&paths, &profile)?;
            write_undo_snapshot(&paths, &p)?;
            p.enable_mod_with_plugins(&store, &mod_id);
            p.save(&paths)?;
            color::success(&format!(
                "Enabled {mod_id} in profile '{profile}' (plugins auto-registered if any)"
            ));
        }

        Commands::Disable { profile, mod_id } => {
            let profile = resolve_profile(&config, profile)?;
            let mut p = Profile::load(&paths, &profile)?;
            write_undo_snapshot(&paths, &p)?;
            p.disable_mod(&mod_id);
            p.save(&paths)?;
            color::success(&format!("Disabled {mod_id} in profile '{profile}'"));
        }

        Commands::EnableMany { profile, mod_ids } => {
            let profile = resolve_profile(&config, profile)?;
            let store = ModStore::load(&paths)?;
            let mut p = Profile::load(&paths, &profile)?;
            write_undo_snapshot(&paths, &p)?;
            let mut n = 0usize;
            for id in mod_ids.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
                p.enable_mod_with_plugins(&store, id);
                n += 1;
            }
            p.save(&paths)?;
            color::success(&format!("Enabled {n} mod(s) in profile '{profile}'"));
        }

        Commands::DisableMany { profile, mod_ids } => {
            let profile = resolve_profile(&config, profile)?;
            let mut p = Profile::load(&paths, &profile)?;
            write_undo_snapshot(&paths, &p)?;
            let mut n = 0usize;
            for id in mod_ids.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
                p.disable_mod(id);
                n += 1;
            }
            p.save(&paths)?;
            color::success(&format!("Disabled {n} mod(s) in profile '{profile}'"));
        }

        Commands::EnableAll { profile } => {
            let profile = resolve_profile(&config, profile)?;
            let store = ModStore::load(&paths)?;
            let mut p = Profile::load(&paths, &profile)?;
            write_undo_snapshot(&paths, &p)?;
            let ids: Vec<String> = store.mods.iter().map(|m| m.id.clone()).collect();
            for id in &ids {
                p.enable_mod_with_plugins(&store, id);
            }
            p.save(&paths)?;
            color::success(&format!("Enabled all {} mod(s) in profile '{profile}'", ids.len()));
        }

        Commands::DisableAll { profile } => {
            let profile = resolve_profile(&config, profile)?;
            let mut p = Profile::load(&paths, &profile)?;
            write_undo_snapshot(&paths, &p)?;
            let ids: Vec<String> = p.mod_order.iter().map(|e| e.mod_id.clone()).collect();
            for id in &ids {
                p.disable_mod(id);
            }
            p.save(&paths)?;
            color::success(&format!("Disabled all {} mod(s) in profile '{profile}'", ids.len()));
        }

        Commands::Reorder {
            profile,
            mod_id,
            index,
        } => {
            let profile = resolve_profile(&config, profile)?;
            let mut p = Profile::load(&paths, &profile)?;
            write_undo_snapshot(&paths, &p)?;
            p.reorder(&mod_id, index);
            p.save(&paths)?;
            color::success(&format!(
                "Reordered {mod_id} to position {index} in '{profile}'"
            ));
        }

        Commands::ExportProfile { profile } => {
            let profile = resolve_profile(&config, profile)?;
            let store = ModStore::load(&paths)?;
            let p = Profile::load(&paths, &profile)?;
            println!("{}", p.export_readable(&store));
        }

        Commands::ImportPlugins {
            profile,
            plugins_txt,
        } => {
            let profile = resolve_profile(&config, profile)?;
            let mut p = Profile::load(&paths, &profile)?;
            p.import_plugins_txt(&plugins_txt)?;
            p.save(&paths)?;
            color::success(&format!(
                "Imported plugin order from {}",
                plugins_txt.display()
            ));
        }

        Commands::Conflicts { profile } => {
            let profile = resolve_profile(&config, profile)?;
            let store = ModStore::load(&paths)?;
            let p = Profile::load(&paths, &profile)?;
            let detailed = vfs::resolve_conflicts_detailed(&store, &p)?;
            let mut any = false;
            for (path, contributions) in &detailed {
                if contributions.len() > 1 {
                    any = true;
                    let winner = contributions
                        .last()
                        .map(|c| c.mod_id.as_str())
                        .unwrap_or("?");
                    println!("{path}");
                    for c in contributions {
                        let name = store
                            .get(&c.mod_id)
                            .map(|m| m.name.as_str())
                            .unwrap_or(&c.mod_id);
                        let marker = if c.mod_id == winner {
                            color::green("WINS")
                        } else {
                            "    ".to_string()
                        };
                        println!("    [{marker}] {name}");
                    }
                }
            }
            if !any {
                color::success(&format!(
                    "No file conflicts between enabled mods in '{profile}'."
                ));
            }

            // BSA awareness: two mods shipping BSAs that both contain the
            // same internal path are invisible to the loose-file check
            // above — surface them separately, informationally (no
            // "winner" is asserted; see bsa::find_bsa_overlaps doc comment
            // for why).
            let mod_bsas: Vec<(String, Vec<(String, bsa::BsaArchive)>)> = p
                .enabled_mods_in_order()
                .filter_map(|mod_id| {
                    let entry = store.get(mod_id)?;
                    let archives = bsa::scan_mod_bsas(&entry.content_dir);
                    if archives.is_empty() {
                        None
                    } else {
                        Some((mod_id.to_string(), archives))
                    }
                })
                .collect();
            if !mod_bsas.is_empty() {
                let overlaps = bsa::find_bsa_overlaps(&mod_bsas);
                if !overlaps.is_empty() {
                    println!("\nBSA content overlaps (informational — no automatic winner):");
                    for (path, contributors) in &overlaps {
                        println!("{path}");
                        for (mod_id, bsa_name) in contributors {
                            let name = store.get(mod_id).map(|m| m.name.as_str()).unwrap_or(mod_id);
                            println!("        {name} ({bsa_name})");
                        }
                    }
                }
            }
        }

        Commands::Validate { profile } => {
            let profile = resolve_profile(&config, profile)?;
            let store = ModStore::load(&paths)?;
            let p = Profile::load(&paths, &profile)?;
            let plugin_paths = build_plugin_path_lookup(&store, &p);
            let problems = validate::check_missing_masters(&p.plugin_order, |plugin| {
                plugin_paths.get(&plugin.to_lowercase()).cloned()
            });
            if problems.is_empty() {
                color::success(&format!(
                    "No missing masters detected for '{profile}'."
                ));
            } else {
                for problem in problems {
                    color::error(&format!("{} is missing master(s):", problem.plugin));
                    for m in problem.missing {
                        println!("    {m}");
                    }
                }
                bail!("validation failed for profile '{profile}'");
            }
        }

        Commands::Cycles { profile } => {
            let profile = resolve_profile(&config, profile)?;
            let store = ModStore::load(&paths)?;
            let p = Profile::load(&paths, &profile)?;
            let plugin_paths = build_plugin_path_lookup(&store, &p);
            let cycles = validate::find_cycles(&p.plugin_order, |plugin| {
                plugin_paths.get(&plugin.to_lowercase()).cloned()
            });
            if cycles.is_empty() {
                color::success(&format!(
                    "No circular master dependencies in '{profile}'."
                ));
            } else {
                for cycle in &cycles {
                    color::error(&format!("Circular dependency: {}", cycle.join(" -> ")));
                }
                bail!("{} circular dependency chain(s) found in '{profile}'", cycles.len());
            }
        }

        Commands::Sort { profile, dry_run } => {
            let profile = resolve_profile(&config, profile)?;
            let store = ModStore::load(&paths)?;
            let mut p = Profile::load(&paths, &profile)?;
            let plugin_paths = build_plugin_path_lookup(&store, &p);
            let sorted = validate::sort_plugins(&p.plugin_order, |plugin| {
                plugin_paths.get(&plugin.to_lowercase()).cloned()
            });
            match sorted {
                Ok(new_order) => {
                    let changed = new_order != p.plugin_order;
                    for plugin in &new_order {
                        println!("  {plugin}");
                    }
                    if !changed {
                        color::success("Already in a valid master-dependency order.");
                    } else if dry_run {
                        color::info("Dry-run — order shown above, nothing saved.");
                    } else {
                        write_undo_snapshot(&paths, &p)?;
                        p.plugin_order = new_order;
                        p.save(&paths)?;
                        color::success(&format!("Sorted and saved '{profile}'."));
                    }
                }
                Err(cycles) => {
                    for cycle in &cycles {
                        color::error(&format!("Circular dependency: {}", cycle.join(" -> ")));
                    }
                    bail!(
                        "cannot sort '{profile}': {} circular dependency chain(s) found — \
                         resolve them first (see `cycles --profile {profile}`)",
                        cycles.len()
                    );
                }
            }
        }

        Commands::DryRun { profile } => {
            let profile = resolve_profile(&config, profile)?;
            let store = ModStore::load(&paths)?;
            let p = Profile::load(&paths, &profile)?;
            let report = vfs::deploy_dry_run(&store, &p)?;
            println!("Would deploy {} files.", report.total_files);
            if report.conflicts.is_empty() {
                color::success("No conflicts.");
            } else {
                color::warn(&format!(
                    "{} file(s) have conflicts (last listed wins):",
                    report.conflicts.len()
                ));
                for c in &report.conflicts {
                    if let Some(last) = c.last() {
                        println!("  {}", last.relative_path.display());
                    }
                }
            }
        }

        Commands::Deploy {
            profile,
            install_dir,
            my_games_dir,
        } => {
            let profile = resolve_profile(&config, profile)?;
            let store = ModStore::load(&paths)?;
            let p = Profile::load(&paths, &profile)?;
            let game = resolve_game(&mut config, &paths, install_dir, my_games_dir)?;
            let backend = vfs::PlatformBackend;
            let count = vfs::deploy(&paths, &backend, &store, &p, &game)?;
            color::success(&format!(
                "Deployed {count} files for profile '{profile}'"
            ));
        }

        Commands::Restore {
            install_dir,
            my_games_dir,
        } => {
            let game = resolve_game(&mut config, &paths, install_dir, my_games_dir)?;
            let backend = vfs::PlatformBackend;
            vfs::restore(&paths, &backend, &game)?;
            color::success(&format!(
                "Restored vanilla Data folder for {}",
                game.install_dir.display()
            ));
        }

        Commands::SetIni {
            ini_file,
            section,
            key,
            value,
        } => {
            ini::set_ini_value(&ini_file, &section, &key, &value)?;
            color::success(&format!(
                "Set [{section}] {key}={value} in {}",
                ini_file.display()
            ));
        }

        Commands::GetIni {
            ini_file,
            section,
            key,
        } => match ini::get_ini_value(&ini_file, &section, &key) {
            Some(v) => println!("{v}"),
            None => {
                color::info("(not set)");
            }
        },

        Commands::Doctor { markdown } => {
            color::info("Running automated checks — this may take a minute…");
            let report = check::run_all(markdown.as_deref())?;
            print!("{}", report.to_terminal());
            if let Some(path) = &markdown {
                color::info(&format!("Markdown report written to {}", path.display()));
            }
            if !report.passed() {
                std::process::exit(1);
            }
        }

        Commands::Completions { shell } => {
            print_completions(&shell)?;
        }

        Commands::RegisterNxmHandler { binary_path } => {
            let binary = match binary_path {
                Some(p) => p,
                None => std::env::current_exe()
                    .context("could not determine the current executable's path")?,
            };
            let apps_dir = dirs::data_dir()
                .context("could not determine platform data directory")?
                .join("applications");
            let desktop_path = nxm::register_handler(&apps_dir, &binary.to_string_lossy())?;
            color::success(&format!(
                "Registered nxm:// handler at {} (pointing at {})",
                desktop_path.display(),
                binary.display()
            ));
            color::info(
                "If Nexus's 'Mod Manager Download' button still doesn't launch this tool, \
                 you may need to select it manually as the default nxm:// handler in your \
                 desktop environment's settings.",
            );
        }

        Commands::HandleNxm { url } => {
            let link = nxm::parse_nxm_url(&url)?;
            println!("Requested: {} / mod {} / file {}", link.game_domain, link.mod_id, link.file_id);
            color::info(
                "This version doesn't download from Nexus automatically yet — download the \
                 file from your browser, then run `install`/`install-fomod` on it. Full Nexus \
                 API integration (automatic download) is planned for a later version.",
            );
        }

        Commands::ModPath { mod_id, open } => {
            let store = ModStore::load(&paths)?;
            let entry = store.get(&mod_id).context("no such mod id — see list-mods")?;
            println!("{}", entry.content_dir.display());
            if open {
                let opener = if cfg!(target_os = "windows") {
                    "explorer"
                } else if cfg!(target_os = "macos") {
                    "open"
                } else {
                    "xdg-open"
                };
                if std::process::Command::new(opener)
                    .arg(&entry.content_dir)
                    .spawn()
                    .is_err()
                {
                    color::warn(&format!(
                        "Could not launch '{opener}' — the path above is still valid, \
                         open it manually."
                    ));
                }
            }
        }

        Commands::Launch => {
            let game = ensure_active_game(&mut config, &paths)?;
            let (exe, _appid) = game.edition.exe_and_appid();
            let skse_loader = game.install_dir.join("skse64_loader.exe");
            let exe_path = if skse_loader.is_file() { skse_loader } else { game.install_dir.join(exe) };
            if !exe_path.is_file() {
                bail!("expected executable not found at {}", exe_path.display());
            }
            // Stated scope: this spawns the executable directly, which
            // works on native Windows. On Linux, GameInstall's Wine prefix
            // information (`wine_prefix`) is intentionally not persisted
            // to config.json (see the field's #[serde(skip)]), so by the
            // time a game is loaded back from config, this tool no longer
            // knows which Proton version / prefix it came from — correctly
            // launching it through Wine/Proton isn't reliably possible
            // from that persisted state. Rather than guess and risk
            // launching through the wrong prefix, this reports a clear
            // message on Linux failure instead of pretending to succeed.
            match std::process::Command::new(&exe_path).current_dir(&game.install_dir).spawn() {
                Ok(_) => color::success(&format!("Launched {}", exe_path.display())),
                Err(e) => {
                    color::error(&format!("Could not launch {}: {e}", exe_path.display()));
                    if !cfg!(target_os = "windows") {
                        color::info(
                            "This is a Windows executable — on Linux, launch it through Steam \
                             (so it runs under the correct Proton prefix) rather than directly; \
                             this tool doesn't yet persist which prefix a game came from between \
                             sessions to do that automatically.",
                        );
                    }
                }
            }
        }

        Commands::Undo => {
            let slot_path = paths.root.join("undo.json");
            if !slot_path.exists() {
                color::info("Nothing to undo.");
                return Ok(());
            }
            let snapshot: Profile = serde_json::from_str(
                &std::fs::read_to_string(&slot_path)
                    .with_context(|| format!("reading {}", slot_path.display()))?,
            )
            .context("undo snapshot is corrupt")?;
            // Swap: whatever is currently on disk for this profile becomes
            // the new undo snapshot, so a second `undo` undoes the undo.
            if let Ok(current) = Profile::load(&paths, &snapshot.name) {
                std::fs::write(&slot_path, serde_json::to_string_pretty(&current)?)?;
            }
            snapshot.save(&paths)?;
            color::success(&format!("Undid last change to '{}'", snapshot.name));
        }

        Commands::CleanTmp => {
            let mut removed = 0usize;
            if paths.tmp.is_dir() {
                for entry in std::fs::read_dir(&paths.tmp)?.flatten() {
                    let path = entry.path();
                    let result = if path.is_dir() {
                        std::fs::remove_dir_all(&path)
                    } else {
                        std::fs::remove_file(&path)
                    };
                    if result.is_ok() {
                        removed += 1;
                    }
                }
            }
            color::success(&format!("Removed {removed} leftover scratch item(s)."));
        }
    }

    Ok(())
}

/// Resolve which `GameInstall` to act on: explicit --install-dir wins, else
/// fall back to the config's active game.
fn resolve_game(
    config: &mut Config,
    paths: &AppPaths,
    install_dir: Option<PathBuf>,
    my_games_dir: Option<PathBuf>,
) -> Result<GameInstall> {
    if let Some(install_dir) = install_dir {
        let my_games = my_games_dir.unwrap_or_else(|| install_dir.clone());
        return find_skyrim_at(&install_dir, &my_games).context(
            "could not confirm a Skyrim install at the given path \
             (looking for SkyrimSE.exe / TESV.exe / SkyrimVR.exe)",
        );
    }
    ensure_active_game(config, paths)
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

/// Generate shell completion scripts without an extra crate dependency.
fn print_completions(shell: &str) -> Result<()> {
    let commands = [
        "detect-game",
        "list-games",
        "use-game",
        "install",
        "estimate-size",
        "update",
        "remove",
        "list-mods",
        "tag",
        "disk-usage",
        "which-mod",
        "new-profile",
        "list-profiles",
        "clone-profile",
        "rename-profile",
        "delete-profile",
        "enable",
        "disable",
        "enable-many",
        "disable-many",
        "reorder",
        "export-profile",
        "import-plugins",
        "conflicts",
        "validate",
        "dry-run",
        "deploy",
        "restore",
        "set-ini",
        "get-ini",
        "doctor",
        "completions",
    ];
    match shell.to_lowercase().as_str() {
        "bash" => {
            println!(
                r#"# skyrim-modmgr bash completion
_skyrim_modmgr() {{
  local cur cmds
  COMPREPLY=()
  cur="${{COMP_WORDS[COMP_CWORD]}}"
  cmds="{cmds}"
  if [[ $COMP_CWORD -eq 1 ]]; then
    COMPREPLY=( $(compgen -W "$cmds" -- "$cur") )
  fi
}}
complete -F _skyrim_modmgr skyrim-modmgr
"#,
                cmds = commands.join(" ")
            );
        }
        "zsh" => {
            println!(
                r#"#compdef skyrim-modmgr
_skyrim_modmgr() {{
  local -a cmds
  cmds=(
{items}
  )
  _describe 'command' cmds
}}
compdef _skyrim_modmgr skyrim-modmgr
"#,
                items = commands
                    .iter()
                    .map(|c| format!("    '{c}'"))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
        }
        "fish" => {
            println!("# skyrim-modmgr fish completion");
            for c in &commands {
                println!(
                    "complete -c skyrim-modmgr -n \"__fish_use_subcommand\" -a {c} -d '{c}'"
                );
            }
        }
        other => bail!("unknown shell '{other}' — use bash, zsh, or fish"),
    }
    Ok(())
}
