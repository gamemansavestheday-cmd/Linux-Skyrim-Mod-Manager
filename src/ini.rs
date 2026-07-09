//! Minimal `[Section]` / `Key=Value` INI editor for `Skyrim.ini` and
//! `SkyrimPrefs.ini`. Deliberately simple: it preserves the file's existing
//! lines and comments, only touching the one key you ask it to set (adding
//! the section and/or key if they don't exist yet), rather than trying to
//! be a general-purpose INI library.

use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

/// Set `key=value` under `[section]` in the INI file at `path`, creating
/// the section and/or key if needed, and preserving everything else in the
/// file untouched. Section/key matching is case-insensitive, matching how
/// the game itself reads these files.
pub fn set_ini_value(path: &Path, section: &str, key: &str, value: &str) -> Result<()> {
    let original = if path.exists() {
        fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?
    } else {
        String::new()
    };

    let mut lines: Vec<String> = original.lines().map(str::to_string).collect();
    let section_header = format!("[{section}]");

    let section_idx = lines
        .iter()
        .position(|l| l.trim().eq_ignore_ascii_case(&section_header));

    match section_idx {
        Some(idx) => {
            // Find the key within this section (up to the next [Section] or EOF).
            let mut end = lines.len();
            for (i, l) in lines.iter().enumerate().skip(idx + 1) {
                if l.trim_start().starts_with('[') {
                    end = i;
                    break;
                }
            }
            let key_idx = lines[idx + 1..end].iter().position(|l| {
                l.split('=')
                    .next()
                    .map(|k| k.trim().eq_ignore_ascii_case(key))
                    .unwrap_or(false)
            });
            match key_idx {
                Some(offset) => lines[idx + 1 + offset] = format!("{key}={value}"),
                None => lines.insert(end, format!("{key}={value}")),
            }
        }
        None => {
            // Section doesn't exist yet — append it at the end of the file.
            if !lines.is_empty() && lines.last().is_some_and(|l| !l.is_empty()) {
                lines.push(String::new());
            }
            lines.push(section_header);
            lines.push(format!("{key}={value}"));
        }
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, lines.join("\n") + "\n")
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Read a single `key=value` under `[section]`, if present.
pub fn get_ini_value(path: &Path, section: &str, key: &str) -> Option<String> {
    let contents = fs::read_to_string(path).ok()?;
    let section_header = format!("[{section}]");
    let mut in_section = false;
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_section = trimmed.eq_ignore_ascii_case(&section_header);
            continue;
        }
        if in_section {
            if let Some((k, v)) = trimmed.split_once('=') {
                if k.trim().eq_ignore_ascii_case(key) {
                    return Some(v.trim().to_string());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn set_and_get_roundtrip() {
        let path = std::env::temp_dir().join(format!(
            "skyrim-modmgr-ini-{}-{}.ini",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::remove_file(&path);
        set_ini_value(&path, "Display", "iMaxAnisotropy", "16").unwrap();
        assert_eq!(get_ini_value(&path, "Display", "iMaxAnisotropy").as_deref(), Some("16"));
        set_ini_value(&path, "Display", "iMaxAnisotropy", "8").unwrap();
        assert_eq!(get_ini_value(&path, "display", "IMAXANISOTROPY").as_deref(), Some("8"));
        set_ini_value(&path, "Audio", "fVolume", "0.5").unwrap();
        let contents = fs::read_to_string(&path).unwrap();
        assert!(contents.contains("[Display]"));
        assert!(contents.contains("[Audio]"));
        let _ = fs::remove_file(&path);
    }
}
