//! Minimal, easy-to-use GUI: Mods / Profile / Saves / Backup / Deploy tabs.
//!
//! NOTE: this binary depends on egui/eframe's full dependency tree
//! (accesskit, winit, etc.), which requires a reasonably modern stable Rust
//! (1.80+ via rustup). It was written and reviewed carefully but could not
//! be `cargo check`ed in the sandbox this was built in, since that sandbox
//! only had rustc 1.75 available via apt. Build it yourself with:
//!     rustup install stable && cargo run --release --bin skyrim-modmgr-gui

use eframe::egui;
use skyrim_modmgr::{
    app_paths::AppPaths,
    backup::{self, BackupSelection},
    config::Config,
    game::{scan_all_prefixes_for_skyrim, DetectedGame},
    profile::Profile,
    saves::{self, SaveFile},
    store::ModStore,
    vfs,
};
use std::collections::HashSet;
use std::path::PathBuf;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1000.0, 720.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Skyrim Mod Manager",
        options,
        Box::new(|_cc| Box::new(App::new())),
    )
}

enum Tab {
    Mods,
    Profile,
    Saves,
    Backup,
    Deploy,
}

struct App {
    paths: AppPaths,
    store: ModStore,
    config: Config,
    profiles: Vec<String>,
    active_profile: Option<Profile>,
    tab: Tab,
    status: String,

    // Mods tab state
    install_source: String,
    install_name: String,
    install_tags: String,
    mod_search: String,

    // Deploy tab state
    detected: Vec<DetectedGame>,
    show_game_picker: bool,

    // New profile dialog state
    new_profile_name: String,

    // Saves tab state
    save_list: Vec<SaveFile>,
    selected_saves: HashSet<String>,
    saves_loaded: bool,

    // Backup tab / dialog state
    show_backup_intro: bool,
    backup_include_mods: bool,
    backup_include_saves: bool,
    /// When true, user is picking a subset of mods (otherwise "all").
    backup_pick_mods: bool,
    /// When true, user is picking a subset of saves (otherwise "all").
    backup_pick_saves: bool,
    backup_mod_ids: HashSet<String>,
    backup_save_names: HashSet<String>,
    also_zip_backup: bool,
    previous_backups: Vec<(PathBuf, Option<backup::BackupManifest>)>,
}

impl App {
    fn new() -> Self {
        let paths = AppPaths::discover().expect("could not set up app data directories");
        let store = ModStore::load(&paths).unwrap_or_default();
        let config = Config::load(&paths).unwrap_or_default();
        let profiles = Profile::list_all(&paths).unwrap_or_default();
        let active_profile = profiles
            .first()
            .and_then(|name| Profile::load(&paths, name).ok());

        let backup_include_mods = config.backup_include_mods;
        let backup_include_saves = config.backup_include_saves;
        let previous_backups = backup::list_backups(&paths).unwrap_or_default();

        Self {
            paths,
            store,
            config,
            profiles,
            active_profile,
            tab: Tab::Mods,
            status: String::new(),
            install_source: String::new(),
            install_name: String::new(),
            install_tags: String::new(),
            mod_search: String::new(),
            detected: Vec::new(),
            show_game_picker: false,
            new_profile_name: String::new(),
            save_list: Vec::new(),
            selected_saves: HashSet::new(),
            saves_loaded: false,
            show_backup_intro: false,
            backup_include_mods,
            backup_include_saves,
            backup_pick_mods: false,
            backup_pick_saves: false,
            backup_mod_ids: HashSet::new(),
            backup_save_names: HashSet::new(),
            also_zip_backup: false,
            previous_backups,
        }
    }

    fn refresh_profiles(&mut self) {
        self.profiles = Profile::list_all(&self.paths).unwrap_or_default();
    }

    fn refresh_saves(&mut self) {
        self.save_list = self
            .config
            .active_game()
            .and_then(|g| saves::list_saves(g).ok())
            .unwrap_or_default();
        self.selected_saves
            .retain(|n| self.save_list.iter().any(|s| &s.name == n));
        self.saves_loaded = true;
    }

    fn refresh_backup_list(&mut self) {
        self.previous_backups = backup::list_backups(&self.paths).unwrap_or_default();
    }

    fn persist_backup_prefs(&mut self) {
        self.config.backup_include_mods = self.backup_include_mods;
        self.config.backup_include_saves = self.backup_include_saves;
        let _ = self.config.save(&self.paths);
    }

    fn run_backup(&mut self) {
        let selection = BackupSelection {
            include_mods: self.backup_include_mods,
            mod_ids: if self.backup_include_mods && self.backup_pick_mods {
                self.backup_mod_ids.iter().cloned().collect()
            } else {
                Vec::new() // empty = all when include_mods
            },
            include_saves: self.backup_include_saves,
            save_names: if self.backup_include_saves && self.backup_pick_saves {
                self.backup_save_names.iter().cloned().collect()
            } else {
                Vec::new()
            },
        };
        let game = self.config.active_game().cloned();
        match backup::create_backup(&self.paths, &self.store, game.as_ref(), &selection) {
            Ok(result) => {
                let mut msg = format!(
                    "Backup complete: {} mod(s), {} save(s), {} file(s) ({}) → {}",
                    result.mods_backed_up,
                    result.saves_backed_up,
                    result.total_files,
                    human_size(result.total_bytes),
                    result.dest.display()
                );
                if self.also_zip_backup {
                    match backup::zip_backup_folder(&result.dest) {
                        Ok(zip_path) => {
                            msg.push_str(&format!("\nAlso zipped to {}", zip_path.display()));
                        }
                        Err(e) => {
                            msg.push_str(&format!("\nZip failed: {e}"));
                        }
                    }
                }
                self.status = msg;
                self.refresh_backup_list();
            }
            Err(e) => self.status = format!("Backup failed: {e}"),
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::TopBottomPanel::top("tabs").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui
                    .selectable_label(matches!(self.tab, Tab::Mods), "Mods")
                    .clicked()
                {
                    self.tab = Tab::Mods;
                }
                if ui
                    .selectable_label(matches!(self.tab, Tab::Profile), "Profile")
                    .clicked()
                {
                    self.tab = Tab::Profile;
                }
                if ui
                    .selectable_label(matches!(self.tab, Tab::Saves), "Saves")
                    .clicked()
                {
                    self.tab = Tab::Saves;
                    if !self.saves_loaded {
                        self.refresh_saves();
                    }
                }
                if ui
                    .selectable_label(matches!(self.tab, Tab::Backup), "Backup")
                    .clicked()
                {
                    self.tab = Tab::Backup;
                    if !self.saves_loaded {
                        self.refresh_saves();
                    }
                }
                if ui
                    .selectable_label(matches!(self.tab, Tab::Deploy), "Deploy")
                    .clicked()
                {
                    self.tab = Tab::Deploy;
                }
                ui.separator();
                // Always-visible Backup button (also in tab strip for discoverability).
                if ui.button("📦 Backup").clicked() {
                    if !self.config.backup_intro_seen {
                        self.show_backup_intro = true;
                    } else {
                        self.tab = Tab::Backup;
                        if !self.saves_loaded {
                            self.refresh_saves();
                        }
                    }
                }
                ui.separator();
                ui.label(&self.status);
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| match self.tab {
            Tab::Mods => self.draw_mods_tab(ui),
            Tab::Profile => self.draw_profile_tab(ui),
            Tab::Saves => self.draw_saves_tab(ui),
            Tab::Backup => self.draw_backup_tab(ui),
            Tab::Deploy => self.draw_deploy_tab(ui),
        });

        if self.show_game_picker {
            egui::Window::new("Which Skyrim install is this?")
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label(
                        "More than one Skyrim install was found. Pick the one you actually \
                         play on — the hint below is how recently its Data folder changed, \
                         which is usually a giveaway for a prefix you've actually used.",
                    );
                    ui.separator();
                    let mut picked: Option<usize> = None;
                    for (i, d) in self.detected.iter().enumerate() {
                        ui.group(|ui| {
                            ui.label(format!("{:?} — {}", d.game.edition, d.source_label));
                            ui.label(
                                egui::RichText::new(d.game.install_dir.display().to_string())
                                    .weak()
                                    .small(),
                            );
                            if ui.button("Use this one").clicked() {
                                picked = Some(i);
                            }
                        });
                    }
                    if let Some(i) = picked {
                        let chosen = self.detected.remove(i);
                        self.config.remember_game(chosen.game);
                        let _ = self.config.save(&self.paths);
                        self.show_game_picker = false;
                        self.status = "Active game updated.".to_string();
                        self.saves_loaded = false;
                    }
                    if ui.button("Cancel").clicked() {
                        self.show_game_picker = false;
                    }
                });
        }

        if self.show_backup_intro {
            egui::Window::new("About Backup")
                .collapsible(false)
                .resizable(true)
                .default_width(480.0)
                .show(ctx, |ui| {
                    ui.heading("What Backup does");
                    ui.label(
                        "Backup copies both your mods and your save games into a timestamped \
                         folder under this app's data directory. That way you can recover if a \
                         deploy goes wrong, a mod update breaks something, or you want a \
                         snapshot before experimenting.",
                    );
                    ui.add_space(8.0);
                    ui.label(
                        "You can turn off backing up mods entirely, or pick only certain mods \
                         and only certain saves. Those choices are on the Backup tab — this \
                         message is only shown once.",
                    );
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new(
                            "Tip: deploy also auto-snapshots saves; this Backup feature is \
                             for full, selective archives you control.",
                        )
                        .weak()
                        .italics(),
                    );
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button("Got it — open Backup").clicked() {
                            self.config.backup_intro_seen = true;
                            let _ = self.config.save(&self.paths);
                            self.show_backup_intro = false;
                            self.tab = Tab::Backup;
                            if !self.saves_loaded {
                                self.refresh_saves();
                            }
                        }
                        if ui.button("Cancel").clicked() {
                            // Still mark seen so we don't nag every click if they cancel;
                            // they can re-open via the Backup tab anytime.
                            self.config.backup_intro_seen = true;
                            let _ = self.config.save(&self.paths);
                            self.show_backup_intro = false;
                        }
                    });
                });
        }
    }
}

impl App {
    fn draw_mods_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("Mod Store");
        ui.label("Every mod you install lives here once, shared across all profiles.");
        ui.label("Supports folders, .zip, .7z, .tar/.tar.gz, or a single loose file.");
        ui.separator();

        ui.horizontal(|ui| {
            ui.label("Folder or archive path:");
            ui.text_edit_singleline(&mut self.install_source);
            if ui.button("Browse…").clicked() {
                if let Some(path) = rfd_pick_file_or_folder() {
                    self.install_source = path.to_string_lossy().to_string();
                }
            }
        });
        ui.horizontal(|ui| {
            ui.label("Display name (optional):");
            ui.text_edit_singleline(&mut self.install_name);
        });
        ui.horizontal(|ui| {
            ui.label("Tags (comma-separated, optional):");
            ui.text_edit_singleline(&mut self.install_tags);
        });
        if ui.button("Install mod").clicked() {
            let source = PathBuf::from(&self.install_source);
            let name = if self.install_name.is_empty() {
                None
            } else {
                Some(self.install_name.clone())
            };
            match self.store.install(&self.paths, &source, name) {
                Ok(id) => {
                    for tag in self
                        .install_tags
                        .split(',')
                        .map(|t| t.trim())
                        .filter(|t| !t.is_empty())
                    {
                        let _ = self.store.add_tag(&self.paths, &id, tag);
                    }
                    self.status = format!("Installed mod {id}");
                    self.install_source.clear();
                    self.install_name.clear();
                    self.install_tags.clear();
                }
                Err(e) => self.status = format!("Install failed: {e}"),
            }
        }

        ui.separator();
        ui.horizontal(|ui| {
            ui.label("Search:");
            ui.text_edit_singleline(&mut self.mod_search);
        });
        ui.separator();

        let filter = self.mod_search.to_lowercase();
        egui::ScrollArea::vertical().show(ui, |ui| {
            for m in self.store.mods.clone() {
                if !filter.is_empty()
                    && !m.name.to_lowercase().contains(&filter)
                    && !m.tags.iter().any(|t| t.to_lowercase().contains(&filter))
                {
                    continue;
                }
                ui.horizontal(|ui| {
                    ui.label(&m.name);
                    if !m.tags.is_empty() {
                        ui.label(
                            egui::RichText::new(format!("[{}]", m.tags.join(", "))).weak(),
                        );
                    }
                    ui.label(egui::RichText::new(&m.id).weak().small());
                    if ui.small_button("Remove").clicked() {
                        if let Err(e) = self.store.remove(&self.paths, &m.id) {
                            self.status = format!("Remove failed: {e}");
                        }
                    }
                    if let Some(profile) = &mut self.active_profile {
                        let enabled = profile
                            .mod_order
                            .iter()
                            .find(|e| e.mod_id == m.id)
                            .map(|e| e.enabled)
                            .unwrap_or(false);
                        if ui
                            .small_button(if enabled { "Enabled ✓" } else { "Enable" })
                            .clicked()
                        {
                            if enabled {
                                profile.disable_mod(&m.id);
                            } else {
                                profile.enable_mod_with_plugins(&self.store, &m.id);
                            }
                            let _ = profile.save(&self.paths);
                        }
                    }
                });
            }
        });
    }

    fn draw_profile_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("Profiles");

        ui.horizontal(|ui| {
            ui.label("Active profile:");
            egui::ComboBox::from_id_source("profile_select")
                .selected_text(
                    self.active_profile
                        .as_ref()
                        .map(|p| p.name.clone())
                        .unwrap_or_else(|| "<none>".to_string()),
                )
                .show_ui(ui, |ui| {
                    for name in self.profiles.clone() {
                        if ui.selectable_label(false, &name).clicked() {
                            self.active_profile = Profile::load(&self.paths, &name).ok();
                        }
                    }
                });
        });

        ui.horizontal(|ui| {
            ui.text_edit_singleline(&mut self.new_profile_name);
            if ui.button("New profile").clicked() && !self.new_profile_name.is_empty() {
                let game_id = self
                    .config
                    .active_game()
                    .map(|g| g.id.clone())
                    .unwrap_or_default();
                let profile = Profile::new(&self.new_profile_name, game_id);
                if let Err(e) = profile.save(&self.paths) {
                    self.status = format!("Could not create profile: {e}");
                } else {
                    self.new_profile_name.clear();
                    self.refresh_profiles();
                }
            }
        });

        ui.separator();

        let Some(profile) = &mut self.active_profile else {
            ui.label("No profile selected. Create one above.");
            return;
        };

        ui.label("Load order (bottom wins conflicts). Use ↑/↓ to reorder:");
        egui::ScrollArea::vertical().show(ui, |ui| {
            let order = profile.mod_order.clone();
            for (i, entry) in order.iter().enumerate() {
                ui.horizontal(|ui| {
                    let mod_name = self
                        .store
                        .get(&entry.mod_id)
                        .map(|m| m.name.clone())
                        .unwrap_or_else(|| entry.mod_id.clone());
                    let mut enabled = entry.enabled;
                    if ui.checkbox(&mut enabled, &mod_name).changed() {
                        if enabled {
                            profile.enable_mod_with_plugins(&self.store, &entry.mod_id);
                        } else {
                            profile.disable_mod(&entry.mod_id);
                        }
                    }
                    if ui.small_button("↑").clicked() && i > 0 {
                        profile.reorder(&entry.mod_id, i - 1);
                    }
                    if ui.small_button("↓").clicked() {
                        profile.reorder(&entry.mod_id, i + 1);
                    }
                });
            }
        });

        if ui.button("Save profile").clicked() {
            if let Err(e) = profile.save(&self.paths) {
                self.status = format!("Save failed: {e}");
            } else {
                self.status = "Profile saved.".to_string();
            }
        }
    }

    fn draw_saves_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("Save Manager");
        ui.label("Import and export Skyrim save files (.ess, .skse co-saves, screenshots).");

        if self.config.active_game().is_none() {
            ui.label("No active game — detect an install on the Deploy tab first.");
            return;
        }

        if let Some(dir) = self.config.active_game().and_then(saves::saves_dir) {
            ui.label(
                egui::RichText::new(format!("Saves folder: {}", dir.display()))
                    .weak()
                    .small(),
            );
        }

        ui.horizontal(|ui| {
            if ui.button("Refresh").clicked() {
                self.refresh_saves();
                self.status = format!("{} save file(s) found.", self.save_list.len());
            }
            if ui.button("Select all").clicked() {
                self.selected_saves = self.save_list.iter().map(|s| s.name.clone()).collect();
            }
            if ui.button("Select none").clicked() {
                self.selected_saves.clear();
            }
            if ui.button("Import…").clicked() {
                if let Some(path) = rfd_pick_save_import() {
                    if let Some(game) = self.config.active_game().cloned() {
                        match saves::import_saves(&game, &path) {
                            Ok(n) => {
                                self.status = format!("Imported {n} save file(s).");
                                self.refresh_saves();
                            }
                            Err(e) => self.status = format!("Import failed: {e}"),
                        }
                    }
                }
            }
            if ui.button("Export selected…").clicked() {
                let selected: Vec<&SaveFile> = self
                    .save_list
                    .iter()
                    .filter(|s| self.selected_saves.contains(&s.name))
                    .collect();
                if selected.is_empty() {
                    self.status = "Select at least one save to export.".to_string();
                } else if let Some(dest) = rfd_pick_save_export_dest() {
                    let as_zip = dest
                        .extension()
                        .and_then(|e| e.to_str())
                        .map(|e| e.eq_ignore_ascii_case("zip"))
                        .unwrap_or(false);
                    match saves::export_saves(&selected, &dest, as_zip) {
                        Ok(n) => {
                            self.status =
                                format!("Exported {n} file(s) to {}.", dest.display());
                        }
                        Err(e) => self.status = format!("Export failed: {e}"),
                    }
                }
            }
            if ui.button("Delete selected").clicked() {
                let selected: Vec<&SaveFile> = self
                    .save_list
                    .iter()
                    .filter(|s| self.selected_saves.contains(&s.name))
                    .collect();
                if selected.is_empty() {
                    self.status = "Select at least one save to delete.".to_string();
                } else {
                    match saves::delete_saves(&selected) {
                        Ok(n) => {
                            self.status = format!("Deleted {n} save file(s).");
                            self.refresh_saves();
                        }
                        Err(e) => self.status = format!("Delete failed: {e}"),
                    }
                }
            }
        });

        ui.separator();

        if self.save_list.is_empty() {
            ui.label("No save files found. Import some, or play the game to create saves.");
            return;
        }

        egui::ScrollArea::vertical().show(ui, |ui| {
            for s in &self.save_list {
                ui.horizontal(|ui| {
                    let mut checked = self.selected_saves.contains(&s.name);
                    if ui.checkbox(&mut checked, "").changed() {
                        if checked {
                            self.selected_saves.insert(s.name.clone());
                        } else {
                            self.selected_saves.remove(&s.name);
                        }
                    }
                    ui.label(format!("[{}]", s.kind.label()));
                    ui.label(&s.name);
                    ui.label(
                        egui::RichText::new(human_size(s.size_bytes))
                            .weak()
                            .small(),
                    );
                });
            }
        });
    }

    fn draw_backup_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("Backup");
        ui.label(
            "Create a snapshot of mods and/or saves. Use the toggles and pickers below to \
             control what is included.",
        );

        if !self.config.backup_intro_seen {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("First time? Read the intro first.")
                        .strong()
                        .color(egui::Color32::from_rgb(220, 180, 80)),
                );
                if ui.button("Show intro").clicked() {
                    self.show_backup_intro = true;
                }
            });
        }

        ui.separator();

        let mut include_mods = self.backup_include_mods;
        let mut include_saves = self.backup_include_saves;
        if ui
            .checkbox(&mut include_mods, "Back up mods")
            .on_hover_text("Uncheck to skip the mod store entirely.")
            .changed()
        {
            self.backup_include_mods = include_mods;
            self.persist_backup_prefs();
        }
        if ui
            .checkbox(&mut include_saves, "Back up saves")
            .on_hover_text("Uncheck to skip save games.")
            .changed()
        {
            self.backup_include_saves = include_saves;
            self.persist_backup_prefs();
        }
        ui.checkbox(
            &mut self.also_zip_backup,
            "Also create a .zip of the backup folder",
        );

        ui.separator();

        if self.backup_include_mods {
            ui.group(|ui| {
                ui.label(egui::RichText::new("Mods").strong());
                let mut pick = self.backup_pick_mods;
                if ui
                    .checkbox(&mut pick, "Pick specific mods (otherwise all mods)")
                    .changed()
                {
                    self.backup_pick_mods = pick;
                    if pick && self.backup_mod_ids.is_empty() {
                        // Default to all selected so nothing is surprising.
                        self.backup_mod_ids =
                            self.store.mods.iter().map(|m| m.id.clone()).collect();
                    }
                }
                if self.backup_pick_mods {
                    ui.horizontal(|ui| {
                        if ui.small_button("All mods").clicked() {
                            self.backup_mod_ids =
                                self.store.mods.iter().map(|m| m.id.clone()).collect();
                        }
                        if ui.small_button("No mods").clicked() {
                            self.backup_mod_ids.clear();
                        }
                    });
                    egui::ScrollArea::vertical()
                        .id_source("backup_mods")
                        .max_height(160.0)
                        .show(ui, |ui| {
                            for m in &self.store.mods {
                                let mut checked = self.backup_mod_ids.contains(&m.id);
                                if ui.checkbox(&mut checked, &m.name).changed() {
                                    if checked {
                                        self.backup_mod_ids.insert(m.id.clone());
                                    } else {
                                        self.backup_mod_ids.remove(&m.id);
                                    }
                                }
                            }
                        });
                    ui.label(
                        egui::RichText::new(format!(
                            "{} of {} mod(s) selected",
                            self.backup_mod_ids.len(),
                            self.store.mods.len()
                        ))
                        .weak()
                        .small(),
                    );
                } else {
                    ui.label(format!("All {} mod(s) will be backed up.", self.store.mods.len()));
                }
            });
        }

        if self.backup_include_saves {
            ui.group(|ui| {
                ui.label(egui::RichText::new("Saves").strong());
                if self.config.active_game().is_none() {
                    ui.label("No active game — saves cannot be backed up until you detect one.");
                } else {
                    if !self.saves_loaded {
                        self.refresh_saves();
                    }
                    let mut pick = self.backup_pick_saves;
                    if ui
                        .checkbox(&mut pick, "Pick specific saves (otherwise all saves)")
                        .changed()
                    {
                        self.backup_pick_saves = pick;
                        if pick && self.backup_save_names.is_empty() {
                            self.backup_save_names =
                                self.save_list.iter().map(|s| s.name.clone()).collect();
                        }
                    }
                    if self.backup_pick_saves {
                        ui.horizontal(|ui| {
                            if ui.small_button("All saves").clicked() {
                                self.backup_save_names =
                                    self.save_list.iter().map(|s| s.name.clone()).collect();
                            }
                            if ui.small_button("No saves").clicked() {
                                self.backup_save_names.clear();
                            }
                            if ui.small_button("Refresh list").clicked() {
                                self.refresh_saves();
                            }
                        });
                        egui::ScrollArea::vertical()
                            .id_source("backup_saves")
                            .max_height(160.0)
                            .show(ui, |ui| {
                                for s in &self.save_list {
                                    let mut checked = self.backup_save_names.contains(&s.name);
                                    if ui.checkbox(&mut checked, &s.name).changed() {
                                        if checked {
                                            self.backup_save_names.insert(s.name.clone());
                                        } else {
                                            self.backup_save_names.remove(&s.name);
                                        }
                                    }
                                }
                            });
                        ui.label(
                            egui::RichText::new(format!(
                                "{} of {} save file(s) selected",
                                self.backup_save_names.len(),
                                self.save_list.len()
                            ))
                            .weak()
                            .small(),
                        );
                    } else {
                        ui.label(format!(
                            "All {} save file(s) will be backed up.",
                            self.save_list.len()
                        ));
                    }
                }
            });
        }

        ui.separator();

        let can_run = self.backup_include_mods || self.backup_include_saves;
        if ui
            .add_enabled(can_run, egui::Button::new("Create backup now"))
            .clicked()
        {
            if !self.config.backup_intro_seen {
                self.show_backup_intro = true;
            } else {
                self.run_backup();
            }
        }

        ui.separator();
        ui.heading("Previous backups");
        if ui.button("Refresh backup list").clicked() {
            self.refresh_backup_list();
        }
        if self.previous_backups.is_empty() {
            ui.label("No user backups yet.");
        } else {
            egui::ScrollArea::vertical()
                .id_source("prev_backups")
                .max_height(180.0)
                .show(ui, |ui| {
                    for (path, manifest) in &self.previous_backups {
                        ui.group(|ui| {
                            ui.label(path.display().to_string());
                            if let Some(m) = manifest {
                                ui.label(
                                    egui::RichText::new(format!(
                                        "{} mod(s), {} save(s), {} file(s) ({})",
                                        m.mod_ids.len(),
                                        m.save_names.len(),
                                        m.total_files,
                                        human_size(m.total_bytes)
                                    ))
                                    .weak()
                                    .small(),
                                );
                            }
                        });
                    }
                });
        }
    }

    fn draw_deploy_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("Deploy");

        if let Some(game) = self.config.active_game() {
            ui.label(format!(
                "Active game: {:?} at {}",
                game.edition,
                game.install_dir.display()
            ));
        } else {
            ui.label("No active game yet — click Detect below.");
        }

        if ui.button("Detect game install(s)").clicked() {
            self.detected = scan_all_prefixes_for_skyrim();
            match self.detected.len() {
                0 => self.status = "No Skyrim install found in any known prefix.".to_string(),
                1 => {
                    let d = self.detected.remove(0);
                    self.config.remember_game(d.game);
                    let _ = self.config.save(&self.paths);
                    self.status = "Game detected and set active.".to_string();
                    self.saves_loaded = false;
                }
                _ => self.show_game_picker = true,
            }
        }

        ui.separator();

        let Some(profile) = &self.active_profile else {
            ui.label("Select a profile in the Profile tab first.");
            return;
        };
        let Some(game) = self.config.active_game().cloned() else {
            ui.label("Detect a game install first.");
            return;
        };

        ui.horizontal(|ui| {
            if ui.button("Preview (dry run)").clicked() {
                match vfs::deploy_dry_run(&self.store, profile) {
                    Ok(report) => {
                        self.status = format!(
                            "Would deploy {} files, {} conflict(s).",
                            report.total_files,
                            report.conflicts.len()
                        );
                    }
                    Err(e) => self.status = format!("Preview failed: {e}"),
                }
            }

            if ui.button("Deploy profile").clicked() {
                let backend = vfs::PlatformBackend;
                match vfs::deploy(&self.paths, &backend, &self.store, profile, &game) {
                    Ok(count) => self.status = format!("Deployed {count} files."),
                    Err(e) => self.status = format!("Deploy failed: {e}"),
                }
            }

            if ui.button("Restore vanilla Data").clicked() {
                let backend = vfs::PlatformBackend;
                match vfs::restore(&self.paths, &backend, &game) {
                    Ok(()) => self.status = "Restored vanilla Data folder.".to_string(),
                    Err(e) => self.status = format!("Restore failed: {e}"),
                }
            }
        });
    }
}

fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.2} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

/// Native file/folder picker (folder for already-extracted mods, or a
/// .zip/.7z/.tar/.tar.gz archive file).
fn rfd_pick_file_or_folder() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .add_filter("Mod archive", &["zip", "7z", "tar", "gz", "tgz"])
        .pick_file()
        .or_else(|| rfd::FileDialog::new().pick_folder())
}

fn rfd_pick_save_import() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .add_filter("Saves / zip", &["ess", "skse", "zip", "jpg", "png"])
        .pick_file()
        .or_else(|| rfd::FileDialog::new().pick_folder())
}

fn rfd_pick_save_export_dest() -> Option<PathBuf> {
    // Prefer saving as a zip; user can also pick a folder via save dialog name.
    rfd::FileDialog::new()
        .add_filter("Zip archive", &["zip"])
        .set_file_name("skyrim-saves.zip")
        .save_file()
}
