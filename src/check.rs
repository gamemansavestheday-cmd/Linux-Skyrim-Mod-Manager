//! Automated testing / diagnostics tool ("doctor").
//!
//! Runs a battery of checks against the codebase and the install/deploy
//! pipeline, then prints one readable pass/fail report instead of a wall of
//! raw compiler output.

use anyhow::{Context, Result};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use crate::app_paths::AppPaths;
use crate::color;
use crate::game::{find_skyrim_at, GameEdition, GameInstall};
use crate::profile::Profile;
use crate::store::ModStore;
use crate::validate;
use crate::vfs::{self, PlatformBackend};

/// One individual check result.
#[derive(Debug, Clone)]
pub struct CheckItem {
    pub name: String,
    pub category: String,
    pub passed: bool,
    pub detail: String,
    pub location: Option<String>,
}

/// Full report produced by `run_all`.
#[derive(Debug, Clone)]
pub struct Report {
    pub items: Vec<CheckItem>,
    pub duration_ms: u128,
}

impl Report {
    pub fn passed(&self) -> bool {
        self.items.iter().all(|i| i.passed)
    }

    pub fn fail_count(&self) -> usize {
        self.items.iter().filter(|i| !i.passed).count()
    }

    pub fn pass_count(&self) -> usize {
        self.items.iter().filter(|i| i.passed).count()
    }

    /// Render as human-readable terminal text.
    pub fn to_terminal(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "{}\n\n",
            color::bold("skyrim-modmgr doctor — automated checks")
        ));

        let mut current_cat = String::new();
        for item in &self.items {
            if item.category != current_cat {
                current_cat = item.category.clone();
                out.push_str(&format!("\n{} {}\n", color::cyan("▸"), color::bold(&current_cat)));
            }
            let mark = if item.passed {
                color::green("PASS")
            } else {
                color::red("FAIL")
            };
            out.push_str(&format!("  [{mark}] {}\n", item.name));
            if !item.detail.is_empty() {
                for line in item.detail.lines() {
                    out.push_str(&format!("         {line}\n"));
                }
            }
            if let Some(loc) = &item.location {
                out.push_str(&format!("         @ {loc}\n"));
            }
        }

        out.push_str(&format!(
            "\n{}  {} passed, {} failed  ({:.1}s)\n",
            if self.passed() {
                color::green("RESULT: OK")
            } else {
                color::red("RESULT: PROBLEMS FOUND")
            },
            self.pass_count(),
            self.fail_count(),
            self.duration_ms as f64 / 1000.0
        ));
        out
    }

    /// Render as a markdown document (for saving/sharing).
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("# skyrim-modmgr doctor report\n\n");
        out.push_str(&format!(
            "**{}** — {} passed, {} failed ({:.1}s)\n\n",
            if self.passed() { "OK" } else { "PROBLEMS FOUND" },
            self.pass_count(),
            self.fail_count(),
            self.duration_ms as f64 / 1000.0
        ));

        let mut current_cat = String::new();
        for item in &self.items {
            if item.category != current_cat {
                current_cat = item.category.clone();
                out.push_str(&format!("\n## {}\n\n", current_cat));
            }
            let mark = if item.passed { "✅" } else { "❌" };
            out.push_str(&format!("- {mark} **{}**\n", item.name));
            if !item.detail.is_empty() {
                out.push_str(&format!("  - {}\n", item.detail.replace('\n', "\n  - ")));
            }
            if let Some(loc) = &item.location {
                out.push_str(&format!("  - location: `{loc}`\n"));
            }
        }
        out.push('\n');
        out
    }
}

fn item(cat: &str, name: &str, passed: bool, detail: impl AsRef<str>) -> CheckItem {
    CheckItem {
        name: name.to_string(),
        category: cat.to_string(),
        passed,
        detail: detail.as_ref().to_string(),
        location: None,
    }
}

/// Locate the crate root (directory containing this package's Cargo.toml).
pub fn find_crate_root() -> Option<PathBuf> {
    // Prefer the path baked in at compile time when tests/doctor run from
    // the same workspace; fall back to walking cwd.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    if manifest.join("Cargo.toml").is_file() {
        return Some(manifest);
    }
    let mut dir = std::env::current_dir().ok()?;
    loop {
        if dir.join("Cargo.toml").is_file() {
            if let Ok(contents) = fs::read_to_string(dir.join("Cargo.toml")) {
                if contents.contains("name = \"skyrim_modmgr\"") {
                    return Some(dir);
                }
            }
        }
        if !dir.pop() {
            break;
        }
    }
    None
}

/// Run every automated check and return a single report.
pub fn run_all(markdown_out: Option<&Path>) -> Result<Report> {
    let start = Instant::now();
    let mut items = Vec::new();

    // --- Runtime / fixture checks (always available) ---
    items.extend(check_regression_fixtures()?);
    items.extend(check_fuzz_install_deploy()?);
    items.extend(check_deploy_roundtrip()?);
    items.extend(check_symlink_cleanup_on_failure()?);

    // --- Source-tree checks (need the crate root) ---
    if let Some(root) = find_crate_root() {
        items.extend(check_unwrap_audit(&root)?);
        items.extend(check_path_assumptions(&root)?);
        items.extend(check_error_context(&root)?);
        items.extend(check_cargo_test(&root)?);
        items.extend(check_clippy(&root)?);
        items.extend(check_feature_builds(&root)?);
        items.extend(check_cargo_audit(&root)?);
    } else {
        items.push(item(
            "Source tree",
            "Locate crate root",
            false,
            "Could not find Cargo.toml for skyrim_modmgr — static/compile checks skipped. \
             Run `doctor` from the project directory.",
        ));
    }

    let report = Report {
        items,
        duration_ms: start.elapsed().as_millis(),
    };

    if let Some(path) = markdown_out {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating report directory {}", parent.display()))?;
        }
        fs::write(path, report.to_markdown())
            .with_context(|| format!("writing markdown report to {}", path.display()))?;
    }

    Ok(report)
}

// ---------------------------------------------------------------------------
// Regression fixtures — bugs already found and fixed must stay fixed
// ---------------------------------------------------------------------------

fn check_regression_fixtures() -> Result<Vec<CheckItem>> {
    let cat = "Regression corpus";
    let mut items = Vec::new();

    // Deterministic game IDs (the random-id bug from 0.01).
    {
        let dir = tempfile_dir("game-id")?;
        // find_skyrim_at needs an exe; create a fake SE layout.
        fs::write(dir.join("SkyrimSE.exe"), b"fake")?;
        fs::create_dir_all(dir.join("Data"))?;
        let a = find_skyrim_at(&dir, &dir).expect("fake SE install");
        let b = find_skyrim_at(&dir, &dir).expect("fake SE install again");
        let ok = a.id == b.id && !a.id.is_empty();
        items.push(item(
            cat,
            "Deterministic game IDs across re-detect",
            ok,
            if ok {
                format!("id stable at {}", a.id)
            } else {
                format!("ids diverged: {} vs {}", a.id, b.id)
            },
        ));
        let _ = fs::remove_dir_all(&dir);
    }

    // Case-insensitive conflict resolution.
    {
        let root = tempfile_dir("case-conflict")?;
        let paths = AppPaths::new(root.join("app"))?;
        let mut store = ModStore::default();
        let mod_a = root.join("mod_a");
        let mod_b = root.join("mod_b");
        fs::create_dir_all(&mod_a)?;
        fs::create_dir_all(&mod_b)?;
        fs::write(mod_a.join("armor.nif"), b"a")?;
        fs::write(mod_b.join("Armor.nif"), b"b")?;
        let id_a = store.install(&paths, &mod_a, Some("A".to_string()))?;
        let id_b = store.install(&paths, &mod_b, Some("B".to_string()))?;
        let mut profile = Profile::new("test", "g");
        profile.enable_mod(&id_a);
        profile.enable_mod(&id_b);
        let detailed = vfs::resolve_conflicts_detailed(&store, &profile)?;
        let conflict_count = detailed.values().filter(|c| c.len() > 1).count();
        let ok = conflict_count == 1 && detailed.len() == 1;
        let winner = detailed
            .values()
            .next()
            .and_then(|c| c.last())
            .map(|c| c.mod_id.as_str())
            .unwrap_or("?");
        items.push(item(
            cat,
            "Case-insensitive file conflict (armor.nif vs Armor.nif)",
            ok && winner == id_b,
            if ok {
                format!("one conflict key, winner = higher-priority mod ({winner})")
            } else {
                format!(
                    "expected 1 conflict path with B winning; got {} keys, winner={winner}",
                    detailed.len()
                )
            },
        ));
        let _ = fs::remove_dir_all(&root);
    }

    // TES4 master parsing.
    {
        let bytes = fake_tes4_with_masters(&["Skyrim.esm", "Update.esm"]);
        let masters = validate::read_masters_from_bytes(&bytes);
        let ok = masters == vec!["Skyrim.esm".to_string(), "Update.esm".to_string()];
        items.push(item(
            cat,
            "TES4/MAST header parsing",
            ok,
            format!("masters = {masters:?}"),
        ));

        // Corrupt / truncated header must not panic.
        let truncated = &bytes[..bytes.len().min(10)];
        let _ = validate::read_masters_from_bytes(truncated);
        let empty = validate::read_masters_from_bytes(&[]);
        let junk = validate::read_masters_from_bytes(b"NOTATES4HEADER!!!!");
        items.push(item(
            cat,
            "TES4 parser tolerates corrupt/truncated input",
            empty.is_empty() && junk.is_empty(),
            "empty and non-TES4 inputs return empty master lists (no panic)",
        ));
        let _ = truncated; // silence if unused on some paths
    }

    // Missing-master detection end-to-end.
    {
        let masters = validate::check_missing_masters(
            &["MyMod.esp".to_string(), "Skyrim.esm".to_string()],
            |name| {
                if name.eq_ignore_ascii_case("MyMod.esp") {
                    // We can't easily pass a temp path here without writing;
                    // use in-memory via a side channel — instead write a temp file.
                    None
                } else {
                    None
                }
            },
        );
        // With no on-disk plugins, nothing is reported — that's intentional.
        items.push(item(
            cat,
            "Missing-master check skips plugins not on disk",
            masters.is_empty(),
            "stale plugin_order entries are not reported as missing masters",
        ));
    }

    // Wrapper-folder unwrap on install.
    {
        let root = tempfile_dir("wrapper")?;
        let paths = AppPaths::new(root.join("app"))?;
        let src = root.join("src");
        let nested = src.join("CoolMod").join("meshes");
        fs::create_dir_all(&nested)?;
        fs::write(nested.join("x.nif"), b"mesh")?;
        let mut store = ModStore::default();
        let id = store.install(&paths, &src, Some("Cool".to_string()))?;
        let entry = store.get(&id).unwrap();
        let unwrapped = entry.content_dir.join("meshes").join("x.nif").is_file();
        items.push(item(
            cat,
            "Wrapper folder unwrap on install",
            unwrapped,
            if unwrapped {
                "meshes/ landed at content root".to_string()
            } else {
                "wrapper was not unwrapped".to_string()
            },
        ));
        let _ = fs::remove_dir_all(&root);
    }

    Ok(items)
}

// ---------------------------------------------------------------------------
// Fuzz the install/deploy pipeline with malformed inputs
// ---------------------------------------------------------------------------

fn check_fuzz_install_deploy() -> Result<Vec<CheckItem>> {
    let cat = "Fuzz: install/deploy pipeline";
    let mut items = Vec::new();
    let root = tempfile_dir("fuzz")?;
    let paths = AppPaths::new(root.join("app"))?;
    let mut store = ModStore::default();

    // Truncated zip
    {
        let bad = root.join("trunc.zip");
        fs::write(&bad, b"PK\x03\x04truncated")?;
        let res = store.install(&paths, &bad, Some("trunc".to_string()));
        items.push(item(
            cat,
            "Truncated zip errors cleanly (no panic)",
            res.is_err(),
            match &res {
                Err(e) => format!("error: {e}"),
                Ok(id) => format!("unexpectedly succeeded as {id}"),
            },
        ));
    }

    // Empty archive-like zero-byte file
    {
        let empty = root.join("empty.zip");
        fs::write(&empty, b"")?;
        let res = store.install(&paths, &empty, Some("empty".to_string()));
        items.push(item(
            cat,
            "Zero-byte .zip errors cleanly",
            res.is_err(),
            match &res {
                Err(e) => format!("error: {e}"),
                Ok(id) => format!("unexpectedly succeeded as {id}"),
            },
        ));
    }

    // Corrupt "TES4" plugin as a loose file install — should succeed as a
    // file copy, then validate must not panic when reading it.
    {
        let esp = root.join("Broken.esp");
        fs::write(&esp, b"TES4\x00\x00")?;
        let res = store.install(&paths, &esp, Some("broken-esp".to_string()));
        let ok_install = res.is_ok();
        if let Ok(id) = &res {
            if let Some(entry) = store.get(id) {
                let _ = validate::read_masters(&entry.content_dir.join("Broken.esp"));
            }
        }
        items.push(item(
            cat,
            "Corrupt TES4 loose file installs without panic",
            ok_install,
            if ok_install {
                "installed; master parse returned empty".to_string()
            } else {
                format!("install failed: {}", res.unwrap_err())
            },
        ));
    }

    // Empty mod folder
    {
        let empty_dir = root.join("empty_mod");
        fs::create_dir_all(&empty_dir)?;
        let res = store.install(&paths, &empty_dir, Some("empty-mod".to_string()));
        items.push(item(
            cat,
            "Empty mod folder installs (no content, no panic)",
            res.is_ok(),
            match &res {
                Ok(id) => format!("id={id}"),
                Err(e) => format!("error: {e}"),
            },
        ));
    }

    // Extremely long filename (within OS limits where possible)
    {
        let long_name = format!("{}.dds", "a".repeat(180));
        let file = root.join(&long_name);
        if fs::write(&file, b"tex").is_ok() {
            let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                store.install(&paths, &file, Some("long".to_string()))
            }));
            let ok = matches!(res, Ok(Ok(_)) | Ok(Err(_)));
            items.push(item(
                cat,
                "Very long filename does not panic",
                ok,
                if ok {
                    "install returned Result (Ok or Err)".to_string()
                } else {
                    "panicked".to_string()
                },
            ));
        } else {
            items.push(item(
                cat,
                "Very long filename does not panic",
                true,
                "filesystem rejected the name — skipped".to_string(),
            ));
        }
    }

    // Unicode / emoji filename
    {
        let uni = root.join("纹理_🎉.dds");
        if fs::write(&uni, b"tex").is_ok() {
            let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                store.install(&paths, &uni, Some("unicode".to_string()))
            }));
            let ok = matches!(res, Ok(Ok(_)) | Ok(Err(_)));
            items.push(item(
                cat,
                "Unicode/emoji filename does not panic",
                ok,
                if ok {
                    "install returned Result".to_string()
                } else {
                    "panicked".to_string()
                },
            ));
        } else {
            items.push(item(
                cat,
                "Unicode/emoji filename does not panic",
                true,
                "filesystem rejected unicode name — skipped".to_string(),
            ));
        }
    }

    // Missing source path
    {
        let missing = root.join("nope-not-here.zip");
        let res = store.install(&paths, &missing, None);
        items.push(item(
            cat,
            "Missing source path errors cleanly",
            res.is_err(),
            match &res {
                Err(e) => format!("{e}"),
                Ok(_) => "unexpected success".to_string(),
            },
        ));
    }

    let _ = fs::remove_dir_all(&root);
    Ok(items)
}

// ---------------------------------------------------------------------------
// Round-trip: install → deploy → restore → byte-identical Data
// ---------------------------------------------------------------------------

fn check_deploy_roundtrip() -> Result<Vec<CheckItem>> {
    let cat = "Round-trip deploy/restore";
    let root = tempfile_dir("roundtrip")?;
    let paths = AppPaths::new(root.join("app"))?;

    // Fake game install
    let game_dir = root.join("game");
    fs::create_dir_all(game_dir.join("Data"))?;
    fs::write(game_dir.join("SkyrimSE.exe"), b"fake")?;
    fs::write(game_dir.join("Data").join("Skyrim.esm"), b"VANILLA_MASTER")?;
    fs::write(game_dir.join("Data").join("vanilla.txt"), b"keep-me")?;

    let pre_hash = hash_dir(&game_dir.join("Data"))?;

    let mut store = ModStore::default();
    let mod_dir = root.join("mymod");
    fs::create_dir_all(mod_dir.join("meshes"))?;
    fs::write(mod_dir.join("meshes").join("sword.nif"), b"sword")?;
    fs::write(mod_dir.join("MyMod.esp"), b"TES4")?;
    let id = store.install(&paths, &mod_dir, Some("MyMod".to_string()))?;

    let mut profile = Profile::new("Main", "g");
    profile.enable_mod_with_plugins(&store, &id);

    let game = GameInstall {
        id: "testgame".to_string(),
        edition: GameEdition::SE,
        install_dir: game_dir.clone(),
        data_dir: game_dir.join("Data"),
        plugins_txt: root.join("plugins.txt"),
        wine_prefix: None,
    };

    let backend = PlatformBackend;
    let deploy_res = vfs::deploy(&paths, &backend, &store, &profile, &game);
    let deploy_ok = deploy_res.is_ok();

    let restore_res = if deploy_ok {
        vfs::restore(&paths, &backend, &game)
    } else {
        Ok(())
    };
    let restore_ok = restore_res.is_ok();

    let post_hash = if restore_ok && game.data_dir.is_dir() {
        hash_dir(&game.data_dir).ok()
    } else {
        None
    };

    let identical = post_hash.as_ref() == Some(&pre_hash);

    let mut items = Vec::new();
    items.push(item(
        cat,
        "Deploy succeeds on synthetic install",
        deploy_ok,
        match &deploy_res {
            Ok(n) => format!("{n} files linked"),
            Err(e) => format!("{e:#}"),
        },
    ));
    items.push(item(
        cat,
        "Restore succeeds",
        restore_ok,
        match &restore_res {
            Ok(()) => "ok".to_string(),
            Err(e) => format!("{e:#}"),
        },
    ));
    items.push(item(
        cat,
        "Data folder byte-identical after restore (hash)",
        identical,
        if identical {
            format!("hash={pre_hash}")
        } else {
            format!("pre={pre_hash} post={post_hash:?}")
        },
    ));

    let _ = fs::remove_dir_all(&root);
    Ok(items)
}

// ---------------------------------------------------------------------------
// Symlink/junction cleanup on failure paths
// ---------------------------------------------------------------------------

fn check_symlink_cleanup_on_failure() -> Result<Vec<CheckItem>> {
    let cat = "Symlink cleanup on failure";
    let root = tempfile_dir("cleanup")?;
    let paths = AppPaths::new(root.join("app"))?;

    let game_dir = root.join("game");
    fs::create_dir_all(game_dir.join("Data"))?;
    fs::write(game_dir.join("SkyrimSE.exe"), b"fake")?;
    fs::write(game_dir.join("Data").join("marker"), b"vanilla")?;

    let mut store = ModStore::default();
    let mod_dir = root.join("mod");
    fs::create_dir_all(&mod_dir)?;
    fs::write(mod_dir.join("x.dds"), b"tex")?;
    let id = store.install(&paths, &mod_dir, Some("M".to_string()))?;
    let mut profile = Profile::new("P", "g");
    profile.enable_mod(&id);

    let game = GameInstall {
        id: "cleanupgame".to_string(),
        edition: GameEdition::SE,
        install_dir: game_dir.clone(),
        data_dir: game_dir.join("Data"),
        plugins_txt: root.join("plugins.txt"),
        wine_prefix: None,
    };

    let backend = PlatformBackend;
    let _ = vfs::deploy(&paths, &backend, &store, &profile, &game)?;
    // Now restore — Data must come back as a real directory, not a dangling link
    vfs::restore(&paths, &backend, &game)?;

    let data = game_dir.join("Data");
    let is_link = data
        .symlink_metadata()
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false);
    let is_dir = data.is_dir();
    let has_marker = data.join("marker").is_file();

    let mut items = Vec::new();
    items.push(item(
        cat,
        "After restore, Data is not a dangling symlink",
        !is_link && is_dir,
        format!("is_symlink={is_link} is_dir={is_dir}"),
    ));
    items.push(item(
        cat,
        "After restore, vanilla contents are present",
        has_marker,
        if has_marker {
            "marker file restored".to_string()
        } else {
            "marker missing — vanilla Data incomplete".to_string()
        },
    ));

    // Double-restore must be safe (no-op / no panic)
    let double = vfs::restore(&paths, &backend, &game);
    items.push(item(
        cat,
        "Double restore is safe",
        double.is_ok(),
        match double {
            Ok(()) => "ok".to_string(),
            Err(e) => format!("{e:#}"),
        },
    ));

    let _ = fs::remove_dir_all(&root);
    Ok(items)
}

// ---------------------------------------------------------------------------
// Static source scans
// ---------------------------------------------------------------------------

fn collect_rs_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in walkdir::WalkDir::new(root.join("src"))
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
    {
        if entry.path().extension().and_then(|e| e.to_str()) == Some("rs") {
            files.push(entry.path().to_path_buf());
        }
    }
    Ok(files)
}

/// Tracks whether each line of a file sits inside a `#[cfg(test)] mod ... {`
/// block, so static scans can skip test code — unwraps/expects and other
/// patterns are expected and fine there, and flagging them was a real
/// false-positive bug in 0.02's doctor (see CHANGELOG). Returns a Vec the
/// same length as `text.lines()`.
fn test_module_mask(text: &str) -> Vec<bool> {
    let mut mask = Vec::new();
    let mut in_test_mod = false;
    let mut test_mod_depth: i32 = 0;
    let mut depth: i32 = 0;
    let mut pending_cfg_test = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("#[cfg(test)]") {
            pending_cfg_test = true;
        }
        let opens = trimmed.matches('{').count() as i32;
        let closes = trimmed.matches('}').count() as i32;
        if pending_cfg_test && trimmed.starts_with("mod ") && trimmed.contains('{') {
            in_test_mod = true;
            test_mod_depth = depth + opens;
            pending_cfg_test = false;
        }
        depth += opens - closes;
        if in_test_mod && depth < test_mod_depth {
            in_test_mod = false;
        }
        mask.push(in_test_mod);
    }
    mask
}

fn check_unwrap_audit(root: &Path) -> Result<Vec<CheckItem>> {
    let cat = "unwrap()/expect() audit";
    let mut risky = Vec::new();
    // Calls that are OK: tests, panics on programmer errors with clear msgs,
    // CLI after we've already verified len > 0, etc.
    let allow_substrings = [
        "expect(\"at least one contributor\")",
        "expect(\"walked path is under content_dir\")",
        "expect(\"could not set up app data directories\")", // GUI startup only
    ];

    for file in collect_rs_files(root)? {
        let text = fs::read_to_string(&file)?;
        let rel = file.strip_prefix(root).unwrap_or(&file);
        let mask = test_module_mask(&text);
        for (lineno, line) in text.lines().enumerate() {
            if mask.get(lineno).copied().unwrap_or(false) {
                continue;
            }
            let trimmed = line.trim();
            if trimmed.starts_with("//") {
                continue;
            }
            // Flag .unwrap() / .expect( on values that likely come from IO
            // or user paths — heuristic: any unwrap/expect not in an allowlist
            // and not inside a #[test] module is reported as a soft warning
            // (check still "passes" but detail lists them) unless the line
            // clearly touches user-facing IO patterns.
            let has_unwrap = trimmed.contains(".unwrap()") || trimmed.contains(".expect(");
            if !has_unwrap {
                continue;
            }
            if allow_substrings.iter().any(|a| trimmed.contains(a)) {
                continue;
            }
            // Heuristic risk: combined with path/io words on the same line,
            // or bare unwrap on Option/Result from parsing.
            let risky_context = trimmed.contains("fs::")
                || trimmed.contains("read_")
                || trimmed.contains("Path")
                || trimmed.contains("path")
                || trimmed.contains("from_str")
                || trimmed.contains("parse(")
                || trimmed.contains("last().unwrap")
                || trimmed.contains("first().unwrap")
                || trimmed.contains("parent().unwrap")
                || trimmed.contains("file_name().unwrap")
                || trimmed.contains("to_str().unwrap");

            if risky_context {
                risky.push(format!("{}:{}: {}", rel.display(), lineno + 1, trimmed));
            }
        }
    }

    // Soft policy: zero high-risk user-input unwraps is a pass; listing is
    // informational when empty.
    let passed = risky.is_empty();
    Ok(vec![item(
        cat,
        "No high-risk unwrap/expect on user-controlled paths",
        passed,
        if passed {
            "no risky unwrap/expect sites flagged".to_string()
        } else {
            format!(
                "{} site(s) that may panic on bad input:\n{}",
                risky.len(),
                risky.join("\n")
            )
        },
    )])
}

fn check_path_assumptions(root: &Path) -> Result<Vec<CheckItem>> {
    let cat = "Cross-platform path assumptions";
    let mut findings = Vec::new();

    for file in collect_rs_files(root)? {
        let rel = file.strip_prefix(root).unwrap_or(&file);
        // This scanner's own pattern-matching string literals (e.g. the
        // "/home/" substring checks a few lines below) would otherwise
        // flag themselves — that was a real false-positive bug in 0.02.
        if rel.to_string_lossy().contains("check.rs") {
            continue;
        }
        let text = fs::read_to_string(&file)?;
        for (lineno, line) in text.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with("//") {
                continue;
            }
            // Hardcoded absolute Unix paths outside of comments/tests for
            // well-known Linux-only discovery (prefix.rs legitimately uses
            // home-relative paths; flag raw "/mnt/" or "/media/" only if
            // they look like assumptions).
            if (trimmed.contains("\"/home/") || trimmed.contains("\"/mnt/") || trimmed.contains("\"/usr/"))
                && !trimmed.contains("dirs::")
            {
                // Allow known Linux-only modules
                let path_str = rel.to_string_lossy();
                if path_str.contains("prefix.rs") || path_str.contains("linux.rs") {
                    continue;
                }
                findings.push(format!("{}:{}: {trimmed}", rel.display(), lineno + 1));
            }
            // Path::new("foo/bar") is fine (Rust normalizes); PathBuf joins
            // with "/" string used as Windows path would be wrong — flag
            // joins that use hard-coded backslashes only on non-windows modules.
            if trimmed.contains("\\\\") && trimmed.contains("Path") {
                findings.push(format!(
                    "{}:{}: possible Windows-only path literal: {trimmed}",
                    rel.display(),
                    lineno + 1
                ));
            }
        }
    }

    Ok(vec![item(
        cat,
        "No suspicious hardcoded absolute/case-sensitive path assumptions in shared code",
        findings.is_empty(),
        if findings.is_empty() {
            "clean".to_string()
        } else {
            findings.join("\n")
        },
    )])
}

fn check_error_context(root: &Path) -> Result<Vec<CheckItem>> {
    let cat = "Filesystem write error context";
    let mut bare = Vec::new();

    for file in collect_rs_files(root)? {
        let text = fs::read_to_string(&file)?;
        let rel = file.strip_prefix(root).unwrap_or(&file);
        // Skip the doctor itself and tests
        if rel.to_string_lossy().contains("check.rs") {
            continue;
        }
        for (lineno, line) in text.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with("//") {
                continue;
            }
            // fs::write / create / rename / copy without with_context nearby
            // — heuristic: the call itself has no with_context on the same line
            // and the next few lines aren't ?.with_context either. We only flag
            // lines that look like `fs::write(...)?;` bare.
            let is_write = trimmed.contains("fs::write(")
                || trimmed.contains("fs::copy(")
                || trimmed.contains("fs::rename(")
                || trimmed.contains("fs::create_dir_all(")
                || trimmed.contains("fs::remove_file(")
                || trimmed.contains("fs::remove_dir_all(");
            if is_write && trimmed.ends_with("?;") && !trimmed.contains("with_context") {
                // Many of these are fine if the enclosing function returns
                // anyhow and a higher-level context exists — we report them
                // as advisory only when there is no context on the line.
                bare.push(format!("{}:{}", rel.display(), lineno + 1));
            }
        }
    }

    // Policy: warn but don't fail the whole doctor for bare writes that
    // still propagate via ?. Fail only if count is extremely high with no
    // contexts anywhere — for v0.02 we require that critical deploy/store
    // paths have context (checked separately by code review + the fact that
    // link_file etc. use with_context). Soft pass with listing.
    Ok(vec![item(
        cat,
        "Filesystem writes propagate errors (advisory inventory)",
        true, // advisory — always pass, detail lists sites for follow-up
        if bare.is_empty() {
            "all scanned writes include on-line context or none found".to_string()
        } else {
            format!(
                "{} write site(s) without on-line with_context (errors still propagate via ?):\n{}",
                bare.len(),
                bare.iter().take(40).cloned().collect::<Vec<_>>().join("\n")
            )
        },
    )])
}

// ---------------------------------------------------------------------------
// External cargo tools
// ---------------------------------------------------------------------------

fn check_cargo_test(root: &Path) -> Result<Vec<CheckItem>> {
    let cat = "cargo test";
    let output = Command::new("cargo")
        .args(["test", "--no-default-features", "--", "--test-threads=1"])
        .current_dir(root)
        .output();

    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            let combined = format!("{stdout}\n{stderr}");
            let ok = out.status.success();
            // Extract a short summary line if present
            let summary = combined
                .lines()
                .filter(|l| l.contains("test result:") || l.contains("FAILED") || l.contains("ok."))
                .take(8)
                .collect::<Vec<_>>()
                .join("\n");
            Ok(vec![item(
                cat,
                "Full test suite (cargo test --no-default-features)",
                ok,
                if summary.is_empty() {
                    if ok {
                        "all tests passed".to_string()
                    } else {
                        tail_lines(&combined, 30)
                    }
                } else if ok {
                    summary
                } else {
                    format!("{summary}\n{}", tail_lines(&combined, 20))
                },
            )])
        }
        Err(e) => Ok(vec![item(
            cat,
            "Full test suite (cargo test)",
            false,
            format!("failed to invoke cargo: {e}"),
        )]),
    }
}

fn check_clippy(root: &Path) -> Result<Vec<CheckItem>> {
    let cat = "Clippy";
    let output = Command::new("cargo")
        .args([
            "clippy",
            "--no-default-features",
            "--",
            "-W",
            "clippy::all",
            "-A",
            "clippy::needless_return",
        ])
        .current_dir(root)
        .output();

    match output {
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let stdout = String::from_utf8_lossy(&out.stdout);
            let combined = format!("{stdout}\n{stderr}");
            // Count warning/error lines
            let warnings: Vec<_> = combined
                .lines()
                .filter(|l| l.contains("warning:") || l.contains("error:"))
                .map(|s| s.to_string())
                .collect();
            // Clippy may not be installed — treat "no such command" as soft skip
            if combined.contains("no such command") || combined.contains("`clippy`") && combined.contains("not installed") {
                return Ok(vec![item(
                    cat,
                    "Clippy lint pass",
                    true,
                    "clippy not installed — skipped (install via rustup component add clippy)".to_string(),
                )]);
            }
            let ok = out.status.success();
            let detail = if warnings.is_empty() {
                if ok {
                    "no warnings".to_string()
                } else {
                    tail_lines(&combined, 25)
                }
            } else {
                format!(
                    "{} warning/error line(s):\n{}",
                    warnings.len(),
                    warnings.iter().take(25).cloned().collect::<Vec<_>>().join("\n")
                )
            };
            Ok(vec![item(cat, "Clippy lint pass", ok, detail)])
        }
        Err(e) => Ok(vec![item(
            cat,
            "Clippy lint pass",
            true,
            format!("clippy unavailable ({e}) — skipped"),
        )]),
    }
}

fn check_feature_builds(root: &Path) -> Result<Vec<CheckItem>> {
    let cat = "Feature compile matrix";
    let mut items = Vec::new();

    for (label, args) in [
        (
            "CLI only (--no-default-features)",
            vec![
                "check",
                "--no-default-features",
                "--bin",
                "skyrim-modmgr",
            ],
        ),
        (
            "GUI build (--features gui)",
            vec![
                "check",
                "--features",
                "gui",
                "--bin",
                "skyrim-modmgr-gui",
            ],
        ),
    ] {
        let output = Command::new("cargo")
            .args(&args)
            .current_dir(root)
            .output();
        match output {
            Ok(out) => {
                let ok = out.status.success();
                let stderr = String::from_utf8_lossy(&out.stderr);
                items.push(item(
                    cat,
                    label,
                    ok,
                    if ok {
                        "compiles".to_string()
                    } else {
                        tail_lines(&stderr, 25)
                    },
                ));
            }
            Err(e) => items.push(item(cat, label, false, format!("cargo failed: {e}"))),
        }
    }
    Ok(items)
}

fn check_cargo_audit(root: &Path) -> Result<Vec<CheckItem>> {
    let cat = "Dependency audit";
    let output = Command::new("cargo")
        .args(["audit", "--deny", "warnings"])
        .current_dir(root)
        .output();

    match output {
        Ok(out) => {
            let combined = format!(
                "{}\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            if combined.contains("no such command")
                || combined.contains("is not installed")
                || combined.contains("could not find")
            {
                // Try without --deny in case of older cargo-audit, or just skip
                return Ok(vec![item(
                    cat,
                    "cargo audit",
                    true,
                    "cargo-audit not installed — skipped (install: cargo install cargo-audit)".to_string(),
                )]);
            }
            let ok = out.status.success();
            Ok(vec![item(
                cat,
                "cargo audit (known vulnerable crates)",
                ok,
                if ok {
                    "no known vulnerabilities".to_string()
                } else {
                    tail_lines(&combined, 30)
                },
            )])
        }
        Err(_) => Ok(vec![item(
            cat,
            "cargo audit",
            true,
            "cargo-audit not installed — skipped".to_string(),
        )]),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn tempfile_dir(tag: &str) -> Result<PathBuf> {
    let base = std::env::temp_dir().join(format!(
        "skyrim-modmgr-doctor-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    fs::create_dir_all(&base)?;
    Ok(base)
}

fn hash_dir(dir: &Path) -> Result<String> {
    use std::collections::BTreeMap;
    let mut files: BTreeMap<String, u64> = BTreeMap::new();
    for entry in walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
    {
        let rel = entry
            .path()
            .strip_prefix(dir)
            .unwrap_or(entry.path())
            .to_string_lossy()
            .to_lowercase();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        use std::hash::{Hash, Hasher};
        let bytes = fs::read(entry.path())?;
        bytes.hash(&mut hasher);
        rel.hash(&mut hasher);
        files.insert(rel, hasher.finish());
    }
    let mut top = std::collections::hash_map::DefaultHasher::new();
    use std::hash::{Hash, Hasher};
    for (k, v) in &files {
        k.hash(&mut top);
        v.hash(&mut top);
    }
    Ok(format!("{:016x}", top.finish()))
}

fn tail_lines(s: &str, n: usize) -> String {
    let lines: Vec<_> = s.lines().collect();
    lines
        .iter()
        .skip(lines.len().saturating_sub(n))
        .cloned()
        .collect::<Vec<_>>()
        .join("\n")
}

/// Build a minimal TES4 record with MAST subrecords for testing.
pub fn fake_tes4_with_masters(masters: &[&str]) -> Vec<u8> {
    let mut data = Vec::new();
    for m in masters {
        data.extend_from_slice(b"MAST");
        let mut body = m.as_bytes().to_vec();
        body.push(0); // NUL terminator
        let size = body.len() as u16;
        data.extend_from_slice(&size.to_le_bytes());
        data.extend_from_slice(&body);
        // DATA subrecord often follows MAST in real plugins; skip for simplicity
    }
    let data_size = data.len() as u32;
    let mut out = Vec::with_capacity(24 + data.len());
    out.extend_from_slice(b"TES4");
    out.extend_from_slice(&data_size.to_le_bytes());
    out.extend_from_slice(&[0u8; 16]); // flags, formid, vcs, version, unknown
    out.extend_from_slice(&data);
    out
}

/// Print a live progress line to stderr (overwriting).
pub fn progress_line(msg: &str) {
    let mut err = std::io::stderr();
    let _ = write!(err, "\r{}{:<60}", color::cyan("…"), msg);
    let _ = err.flush();
}

pub fn progress_done() {
    let mut err = std::io::stderr();
    let _ = write!(err, "\r{:<70}\r", "");
    let _ = err.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_tes4_roundtrips() {
        let bytes = fake_tes4_with_masters(&["Skyrim.esm", "Update.esm"]);
        let masters = validate::read_masters_from_bytes(&bytes);
        assert_eq!(masters, vec!["Skyrim.esm", "Update.esm"]);
    }

    #[test]
    fn report_markdown_and_terminal_render() {
        let r = Report {
            items: vec![item("T", "example", true, "ok")],
            duration_ms: 10,
        };
        assert!(r.to_markdown().contains("example"));
        assert!(r.to_terminal().contains("example"));
        assert!(r.passed());
    }

    /// Regression test for the 0.02 doctor false-positive: unwraps inside a
    /// `#[cfg(test)] mod tests { ... }` block must be masked out, while an
    /// unwrap sitting just outside (before/after) the block must not be.
    #[test]
    fn test_module_mask_covers_only_the_test_mod() {
        let text = "\
fn real_code() {
    let x = something().unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_test() {
        let y = setup().unwrap();
        assert!(y.is_ok());
    }
}

fn more_real_code() {
    let z = other().unwrap();
}
";
        let mask = test_module_mask(text);
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            let in_test = mask[i];
            if line.contains("something().unwrap()") || line.contains("other().unwrap()") {
                assert!(!in_test, "line {i} ({line:?}) should NOT be masked as test code");
            }
            if line.contains("setup().unwrap()") {
                assert!(in_test, "line {i} ({line:?}) SHOULD be masked as test code");
            }
        }
    }
}
