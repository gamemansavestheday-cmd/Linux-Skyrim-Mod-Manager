//! Minimal, easy-to-use GUI: Mods / Profile / Deploy tabs.
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
    config::Config,
    game::{scan_all_prefixes_for_skyrim, DetectedGame},
    profile::Profile,
    store::ModStore,
    vfs,
};
use std::path::PathBuf;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([950.0, 700.0]),
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
        }
    }

    fn refresh_profiles(&mut self) {
        self.profiles = Profile::list_all(&self.paths).unwrap_or_default();
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::TopBottomPanel::top("tabs").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.selectable_label(matches!(self.tab, Tab::Mods), "Mods").clicked() {
                    self.tab = Tab::Mods;
                }
                if ui.selectable_label(matches!(self.tab, Tab::Profile), "Profile").clicked() {
                    self.tab = Tab::Profile;
                }
                if ui.selectable_label(matches!(self.tab, Tab::Deploy), "Deploy").clicked() {
                    self.tab = Tab::Deploy;
                }
                ui.separator();
                ui.label(&self.status);
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| match self.tab {
            Tab::Mods => self.draw_mods_tab(ui),
            Tab::Profile => self.draw_profile_tab(ui),
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
                            ui.label(egui::RichText::new(d.game.install_dir.display().to_string()).weak().small());
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
                    }
                    if ui.button("Cancel").clicked() {
                        self.show_game_picker = false;
                    }
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
                    for tag in self.install_tags.split(',').map(|t| t.trim()).filter(|t| !t.is_empty()) {
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
                        ui.label(egui::RichText::new(format!("[{}]", m.tags.join(", "))).weak());
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
                        if ui.small_button(if enabled { "Enabled ✓" } else { "Enable" }).clicked() {
                            if enabled {
                                profile.disable_mod(&m.id);
                            } else {
                                // Also auto-registers any plugins this mod
                                // ships into the profile's plugin order.
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
                let game_id = self.config.active_game().map(|g| g.id.clone()).unwrap_or_default();
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

/// Native file/folder picker (folder for already-extracted mods, or a
/// .zip/.7z/.tar/.tar.gz archive file).
fn rfd_pick_file_or_folder() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .add_filter("Mod archive", &["zip", "7z", "tar", "gz", "tgz"])
        .pick_file()
        .or_else(|| rfd::FileDialog::new().pick_folder())
}
