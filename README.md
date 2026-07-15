# Linux Skyrim Mod Manager

A cross-platform (Linux + Windows) Skyrim mod manager built around a
symlink/junction-based virtual file system, a global mod store, and
per-game profiles works on Steam/PortProton/Lutris/Heroic/Proton/Wine. MIT licensed

<img width="2000" height="1440" alt="image" src="https://github.com/user-attachments/assets/89fb18c0-d35d-4a61-b450-942ca691daa9" />

**Versioning:** this project counts up 0.01, 0.02, 0.03, ... and hits 1.00
at 0.20 (20 releases to a stable 1.0 — pick your own reason for the number).
(keeps the same formatting 1.01 and so on...)
In Cargo/semver terms that's `0.1.0`, `0.2.0`, ... `0.19.0`, then `1.0.0` at
the 20th release, since Cargo requires real semver. This is `0.1.0` / v0.01.

### Prefix & install detection
- Detects Skyrim installs across **Steam/Proton, PortProton, Lutris,
  Heroic Games Launcher, Bottles, CrossOver, plain `~/.wine`, and a custom
  `$WINEPREFIX`** — not just Steam.
- Inside each prefix, figures out which `drive_c/users/<name>` folder is
  actually in use without assuming it's `steamuser` (Proton's default) —
  it could just as easily be your real Linux username (Lutris/Bottles/
  custom prefixes), so detection prefers whichever user folder already has
  Skyrim save/INI data, then `steamuser`, then `$USER`, then whatever's left.
- **If more than one Skyrim install is found**, you're shown all of them —
  edition, source (e.g. "Steam/Proton — Skyrim Special Edition (appid
  489830)" or "PortProton — DEFAULT"), full path, and how recently that
  install's `Data` folder was touched (a good proxy for "which one I
  actually play/mod") — and asked to pick one. The CLI prompts on the spot;
  the GUI pops a picker window. Your choice is remembered (see below), so
  this only has to happen once.
- Reads Steam's `appmanifest_<appid>.acf` so a Steam/Proton prefix shows up
  labeled with the real game name, not just a bare app id, and checks
  `libraryfolders.vdf` so a Skyrim install on a second drive is still found.

<img width="2000" height="1440" alt="image" src="https://github.com/user-attachments/assets/8276c241-5980-43d6-92f8-c9420695453e" />

### Persistence
- Every confirmed game install is remembered in `config.json` (id, edition,
  paths) so `deploy`/`restore` work with **zero flags** after the first
  `detect-game` — no retyping `--install-dir` every time.
- **Bugfix worth flagging explicitly:** game IDs are derived deterministically
  from the install path (a hash), not randomly generated. An earlier version
  of this code generated a random id per detection, which would have
  silently orphaned the vanilla-`Data` backup and broken `restore` the
  moment you re-ran `detect-game` after a reboot. Caught and fixed before
  shipping — see the smoke-test section below for how it was verified.
<img width="3840" height="2026" alt="image" src="https://github.com/user-attachments/assets/da4e5539-e5c8-4b31-bdf4-4fc59477db8b" />

### Mod store — "install anything"
- Installs from an already-extracted **folder**, a **`.zip`**, a **`.7z`**,
  a **`.tar`/`.tar.gz`/`.tgz`**, or a **single loose file** — one `.esp`, one
  texture, one script, whatever. No archive or folder structure required.
- Automatically **unwraps wrapper folders** (extremely common in Nexus
  zips that put everything inside `ModName/` before `meshes/`), so the
  store's content lands at the right level to mirror straight into `Data`.
- **Tags** for organizing/filtering (`--tags textures,armor`), plus
  `list-mods --tag armor`.
- **Update-in-place**: replace a mod's files from a new archive while
  keeping its id, so every profile referencing it keeps working.
- **Disk usage report**: per-mod size, largest first, plus a total — for
  when your SSD mysteriously fills up after 200 texture mods.
- `.rar` is explicitly rejected with a clear message (no MIT-licensed
  pure-Rust RAR decoder exists) rather than silently failing.
<img width="3840" height="2026" alt="image" src="https://github.com/user-attachments/assets/eabdc6a0-49f6-470d-978a-8fece65e7308" />

### Profiles
- Ordered mod list (priority order, last wins on file conflicts) + a
  plugin (`.esp`/`.esm`/`.esl`) load order, written to `plugins.txt` on
  deploy.
- **Auto plugin registration**: enabling a mod also registers any plugins
  it ships into the load order automatically — you don't separately
  maintain a plugin list for every mod you enable.
- Clone / rename / delete profiles; export a human-readable summary (for
  sharing a modlist); import an existing `plugins.txt` (e.g. from MO2/Vortex).
- Removing a mod from the store scrubs it out of every profile that
  referenced it, so profiles never silently point at nothing.

### Safety & diagnostics
- **Conflict viewer** (`conflicts`): every file more than one enabled mod
  provides, and which one wins — matched **case-insensitively**, since
  that's how the game and Windows/Wine actually see filenames (`Armor.nif`
  and `armor.nif` are the same file to Skyrim even though they're two
  different files on a case-sensitive Linux disk — without this, both
  would incorrectly "win" as separate symlinks instead of one properly
  overriding the other).
- **Missing-master validation** (`validate`): parses each enabled plugin's
  `TES4` header (the same trick LOOT/MO2/xEdit use) to check whether every
  master file it requires is actually enabled — catches the #1 cause of a
  Skyrim crash-on-launch before it happens, verified in this session
  against a real synthetic `TES4`/`MAST` header (see testing notes below).
- **Dry-run deploy** (`dry-run`): file count + conflict preview, touches
  nothing on disk.
- **Restore/undo** (`restore`): puts the vanilla `Data` folder back,
  on demand.
- **Save-game backup**: every deploy snapshots the game's `Saves` folder
  into `backups/<game-id>/saves/<timestamp>/` first, so a bad mod
  combination never costs you a save.
- **INI tweak editor** (`set-ini`/`get-ini`): edit a single
  `Skyrim.ini`/`SkyrimPrefs.ini` key without hand-editing the file.

### VFS deploy
- **Linux:** plain symlinks. Wine/Proton resolve them at the host
  filesystem level, so the game process just sees ordinary files.
- **Windows:** NTFS directory junctions for the `Data` mount + hardlinks
  for individual files — **no Administrator or Developer Mode required**.
  Falls back to a plain copy if the mod store and game are on different
  volumes (hardlinks are same-volume only), so a deploy across drives still
  succeeds instead of erroring out.
- The original `Data` folder is renamed into `backups/<game-id>/` the
  *first* time a profile is deployed for that game and restored via
  `restore` — the real install is never overwritten in place, and
  redeploying (including switching profiles) is idempotent.
<img width="3840" height="2026" alt="image" src="https://github.com/user-attachments/assets/2fe16306-474e-47a4-bcbc-e83366f61a96" />

## Project layout

```
src/
  app_paths.rs   — where the app stores its data (mods, profiles, backups)
  config.rs      — persisted "known games" + active game/profile
  store.rs       — global mod store: install/update/remove, tags, disk usage
  profile.rs     — profile struct: mod + plugin load order, clone/rename/export
  prefix.rs      — Wine/Proton/PortProton/Lutris/Heroic/Bottles/CrossOver
                   prefix discovery + user-dir resolution (Linux)
  game.rs        — locate a Skyrim install; scan_all_prefixes_for_skyrim for
                   multi-install disambiguation
  validate.rs    — TES4 header parsing for missing-master detection
  ini.rs         — minimal Skyrim.ini/SkyrimPrefs.ini tweak editor
  vfs/
    mod.rs       — shared conflict-resolution + deploy/dry-run/restore logic
    linux.rs     — symlink backend
    windows.rs   — junction + hardlink backend
  bin/
    cli.rs       — full command-line front end (all features above)
    gui.rs       — egui-based GUI (Mods / Profile / Deploy tabs)
```

## Building

```bash
# CLI only (fewer dependencies, builds on almost any recent stable Rust):
cargo build --release --no-default-features --bin skyrim-modmgr

# GUI too (needs a reasonably modern stable Rust, e.g. 1.80+):
cargo build --release --bin skyrim-modmgr-gui

# Windows cross-compile (from Linux, if you have the target installed):
rustup target add x86_64-pc-windows-gnu
cargo build --release --target x86_64-pc-windows-gnu --bin skyrim-modmgr-gui
```

## CLI usage

```bash
# Detect Skyrim across every known prefix type; asks which one if >1 found
skyrim-modmgr detect-game
skyrim-modmgr list-games
skyrim-modmgr use-game <id-prefix>

# Mods
skyrim-modmgr install ~/Downloads/SomeMod-1234-1-0.zip --name "Some Mod" --tags armor,textures
skyrim-modmgr update <mod-id> ~/Downloads/SomeMod-1235-1-1.zip
skyrim-modmgr list-mods --tag armor
skyrim-modmgr disk-usage

# Profiles
skyrim-modmgr new-profile Main
skyrim-modmgr enable Main <mod-id>       # also auto-registers its plugins
skyrim-modmgr reorder Main <mod-id> 0
skyrim-modmgr clone-profile Main Experimental
skyrim-modmgr export-profile Main

# Safety checks before you commit
skyrim-modmgr conflicts Main
skyrim-modmgr validate Main
skyrim-modmgr dry-run Main

# Deploy / undo (uses the active game — no path flags needed after setup)
skyrim-modmgr deploy Main
skyrim-modmgr restore

# INI tweaks
skyrim-modmgr set-ini "path/to/Skyrim.ini" Display iMaxAnisotropy 16
```
