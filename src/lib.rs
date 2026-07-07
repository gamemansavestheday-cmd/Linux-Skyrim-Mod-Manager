//! skyrim_modmgr — a cross-platform (Linux + Windows) Skyrim mod manager.
//!
//! Core design:
//!  - `store`   : the global mod store. Every installed mod (from a folder,
//!                .zip, or .7z) is extracted exactly once into a content-
//!                addressed folder here. This is the single source of truth.
//!  - `profile` : a profile is an ordered list of (mod_id, enabled) plus a
//!                plugin load order. Profiles reference the store; they never
//!                copy files.
//!  - `vfs`     : deploys a profile's enabled mods on top of the game's Data
//!                folder. On Linux this is done with symlinks. On Windows
//!                this is done with NTFS directory junctions / hardlinks
//!                (no admin rights required). Everything above this layer
//!                (store, profile, conflict resolution) is shared code.
//!  - `prefix`  : (Linux only) locates Wine/Proton prefixes and figures out
//!                which "Windows user" folder inside them is actually the
//!                Skyrim install, regardless of whether it's named
//!                `steamuser` (Proton default) or the real Linux username
//!                (Lutris/bottles/custom prefixes).
//!  - `game`    : locates the Skyrim installation itself (Data folder,
//!                plugins.txt location, INI locations) given a prefix or a
//!                native Windows path.

pub mod app_paths;
pub mod config;
pub mod game;
pub mod ini;
pub mod prefix;
pub mod profile;
pub mod store;
pub mod validate;
pub mod vfs;

pub use app_paths::AppPaths;
