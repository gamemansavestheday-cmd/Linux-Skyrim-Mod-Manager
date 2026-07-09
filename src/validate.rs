//! Lightweight Skyrim plugin (.esp/.esm/.esl) header parsing — just enough
//! to answer "what master files does this plugin require", so we can warn
//! about missing masters before the game does (as a crash on launch).
//!
//! This does NOT parse the full plugin format. It only reads the top-level
//! `TES4` record (always the first record in the file) and walks its
//! subrecords looking for `MAST` (master filename) chunks. That's the same
//! trick every load-order tool (LOOT, MO2, xEdit) relies on for this
//! specific question.
//!
//! Record header layout (Skyrim SE/AE, 24 bytes):
//!   [0..4)   signature ("TES4")
//!   [4..8)   data size (u32 LE) — length of the subrecord data that follows
//!   [8..12)  record flags
//!   [12..16) form id
//!   [16..20) version control info
//!   [20..22) form version
//!   [22..24) unknown
//! Subrecord layout:
//!   [0..4)   tag (e.g. "MAST", "CNAM")
//!   [4..6)   size (u16 LE)
//!   [6..6+size) data — for MAST, a null-terminated ASCII/UTF-8 string

use std::fs;
use std::path::Path;

/// Read the list of master plugin filenames a single .esp/.esm/.esl
/// requires. Returns an empty list (rather than erroring) for anything that
/// doesn't parse cleanly — a corrupt or unusual plugin shouldn't crash the
/// whole validation pass, it should just be silently skipped.
pub fn read_masters(plugin_path: &Path) -> Vec<String> {
    let Ok(bytes) = fs::read(plugin_path) else {
        return Vec::new();
    };
    read_masters_from_bytes(&bytes)
}

/// Parse masters from an already-loaded plugin byte buffer. Public so the
/// doctor/fuzz harness and unit tests can feed synthetic/corrupt headers
/// without touching the filesystem.
pub fn read_masters_from_bytes(bytes: &[u8]) -> Vec<String> {
    const HEADER_LEN: usize = 24;
    if bytes.len() < HEADER_LEN || &bytes[0..4] != b"TES4" {
        return Vec::new();
    }
    let data_size = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
    let start = HEADER_LEN;
    let end = (start + data_size).min(bytes.len());
    if end <= start {
        return Vec::new();
    }

    let mut masters = Vec::new();
    let mut i = start;
    while i + 6 <= end {
        let tag = &bytes[i..i + 4];
        let size = u16::from_le_bytes([bytes[i + 4], bytes[i + 5]]) as usize;
        let data_start = i + 6;
        let data_end = (data_start + size).min(end);
        if tag == b"MAST" {
            let raw = &bytes[data_start..data_end];
            // Strip trailing NUL terminator(s).
            let trimmed = raw
                .split(|&b| b == 0)
                .next()
                .unwrap_or(raw);
            if let Ok(name) = std::str::from_utf8(trimmed) {
                if !name.is_empty() {
                    masters.push(name.to_string());
                }
            }
        }
        i = data_end;
    }
    masters
}

/// One plugin's missing-master problem: the plugin that needs a master, and
/// which required master(s) aren't in the enabled plugin list.
#[derive(Debug, Clone)]
pub struct MissingMasters {
    pub plugin: String,
    pub missing: Vec<String>,
}

/// Check every enabled plugin's masters against the full enabled-plugin
/// list (case-insensitively, matching Skyrim's own filename matching).
/// `find_plugin_path` should locate a given plugin's file on disk in the
/// mod store (e.g. by scanning all enabled mods' content dirs) so we can
/// actually read its header — plugins not found on disk are skipped rather
/// than reported as broken, since that's a different problem (a stale
/// plugin_order entry) from a genuine missing master.
pub fn check_missing_masters(
    enabled_plugins: &[String],
    find_plugin_path: impl Fn(&str) -> Option<std::path::PathBuf>,
) -> Vec<MissingMasters> {
    let enabled_lower: Vec<String> = enabled_plugins.iter().map(|p| p.to_lowercase()).collect();
    let mut problems = Vec::new();

    for plugin in enabled_plugins {
        let Some(path) = find_plugin_path(plugin) else {
            continue;
        };
        let masters = read_masters(&path);
        let missing: Vec<String> = masters
            .into_iter()
            .filter(|m| !enabled_lower.contains(&m.to_lowercase()))
            .collect();
        if !missing.is_empty() {
            problems.push(MissingMasters {
                plugin: plugin.clone(),
                missing,
            });
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_tes4(masters: &[&str]) -> Vec<u8> {
        let mut data = Vec::new();
        for m in masters {
            data.extend_from_slice(b"MAST");
            let mut body = m.as_bytes().to_vec();
            body.push(0);
            data.extend_from_slice(&(body.len() as u16).to_le_bytes());
            data.extend_from_slice(&body);
        }
        let mut out = Vec::new();
        out.extend_from_slice(b"TES4");
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&[0u8; 16]);
        out.extend_from_slice(&data);
        out
    }

    #[test]
    fn reads_multiple_masters() {
        let bytes = fake_tes4(&["Skyrim.esm", "Update.esm", "Dawnguard.esm"]);
        assert_eq!(
            read_masters_from_bytes(&bytes),
            vec!["Skyrim.esm", "Update.esm", "Dawnguard.esm"]
        );
    }

    #[test]
    fn rejects_non_tes4_and_truncated() {
        assert!(read_masters_from_bytes(b"").is_empty());
        assert!(read_masters_from_bytes(b"XXXX").is_empty());
        assert!(read_masters_from_bytes(b"TES").is_empty());
        let mut bytes = fake_tes4(&["Skyrim.esm"]);
        bytes.truncate(10);
        assert!(read_masters_from_bytes(&bytes).is_empty());
    }

    #[test]
    fn missing_masters_case_insensitive() {
        let dir = std::env::temp_dir().join(format!(
            "skyrim-modmgr-validate-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let esp = dir.join("MyMod.esp");
        std::fs::write(&esp, fake_tes4(&["Skyrim.esm", "Update.esm"])).unwrap();

        let problems = check_missing_masters(
            &["MyMod.esp".into(), "skyrim.esm".into()],
            |name| {
                if name.eq_ignore_ascii_case("MyMod.esp") {
                    Some(esp.clone())
                } else {
                    None
                }
            },
        );
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].missing, vec!["Update.esm"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
