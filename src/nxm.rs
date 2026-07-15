//! Nexus Mods `nxm://` URL handling and Linux desktop-handler registration.
//!
//! Full automatic download (Nexus API + API key) is intentionally not in this
//! version — we parse the link so `handle-nxm` can show what was requested,
//! and we can register a `.desktop` file so "Mod Manager Download" opens us.

use anyhow::{bail, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// A parsed `nxm://` download link from Nexus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NxmLink {
    /// Game domain slug, e.g. `skyrimspecialedition`.
    pub game_domain: String,
    /// Nexus mod id.
    pub mod_id: u64,
    /// Nexus file id within that mod.
    pub file_id: u64,
    /// Optional key/expires query params (present on recent links).
    pub key: Option<String>,
    pub expires: Option<String>,
}

/// Parse an `nxm://` URL.
///
/// Supported shapes (case-insensitive scheme):
/// - `nxm://skyrimspecialedition/mods/12345/files/67890`
/// - `nxm://skyrimspecialedition/mods/12345/files/67890?key=...&expires=...`
pub fn parse_nxm_url(url: &str) -> Result<NxmLink> {
    let url = url.trim();
    let lower = url.to_lowercase();
    if !lower.starts_with("nxm://") {
        bail!("not an nxm:// URL: {url}");
    }
    let rest = &url[6..]; // keep original case for path, scheme already matched
    let (path_part, query) = match rest.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (rest, None),
    };
    // path: <game>/mods/<mod_id>/files/<file_id>
    let segments: Vec<&str> = path_part.split('/').filter(|s| !s.is_empty()).collect();
    if segments.len() < 4 || !segments[1].eq_ignore_ascii_case("mods") || !segments[3].eq_ignore_ascii_case("files")
    {
        // Some links include an extra empty segment or trailing slash variants.
        if segments.len() >= 5
            && segments[1].eq_ignore_ascii_case("mods")
            && segments[3].eq_ignore_ascii_case("files")
        {
            // fall through with standard layout
        } else {
            bail!(
                "unrecognized nxm URL shape (expected nxm://<game>/mods/<id>/files/<id>): {url}"
            );
        }
    }
    let game_domain = segments[0].to_string();
    let mod_id: u64 = segments[2]
        .parse()
        .with_context(|| format!("invalid mod id '{}'", segments[2]))?;
    // Standard layout: game / mods / <mod_id> / files / <file_id>
    let file_id_str = if segments.len() >= 5 {
        segments[4]
    } else if segments.len() >= 4 {
        segments[3] // rare truncated form
    } else {
        bail!("missing file id in nxm URL: {url}");
    };
    let file_id: u64 = file_id_str
        .parse()
        .with_context(|| format!("invalid file id '{file_id_str}' in {url}"))?;

    let mut key = None;
    let mut expires = None;
    if let Some(q) = query {
        for pair in q.split('&') {
            if let Some((k, v)) = pair.split_once('=') {
                match k {
                    "key" => key = Some(v.to_string()),
                    "expires" => expires = Some(v.to_string()),
                    _ => {}
                }
            }
        }
    }

    Ok(NxmLink {
        game_domain,
        mod_id,
        file_id,
        key,
        expires,
    })
}

/// Write a FreeDesktop `.desktop` entry that handles `x-scheme-handler/nxm`
/// and point it at `binary_path handle-nxm %u`. Best-effort: also runs
/// `xdg-mime default … x-scheme-handler/nxm` when available.
///
/// Returns the path of the written `.desktop` file.
pub fn register_handler(apps_dir: &Path, binary_path: &str) -> Result<PathBuf> {
    fs::create_dir_all(apps_dir)
        .with_context(|| format!("creating applications dir {}", apps_dir.display()))?;
    let desktop_path = apps_dir.join("skyrim-modmgr-nxm.desktop");
    let contents = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Skyrim Mod Manager (Nexus Download)\n\
         Exec=\"{binary_path}\" handle-nxm %u\n\
         StartupNotify=false\n\
         NoDisplay=true\n\
         MimeType=x-scheme-handler/nxm;\n\
         Categories=Game;\n"
    );
    fs::write(&desktop_path, contents)
        .with_context(|| format!("writing {}", desktop_path.display()))?;

    // Best-effort MIME registration; ignore failures (e.g. headless systems).
    let _ = std::process::Command::new("xdg-mime")
        .args([
            "default",
            "skyrim-modmgr-nxm.desktop",
            "x-scheme-handler/nxm",
        ])
        .status();
    let _ = std::process::Command::new("update-desktop-database")
        .arg(apps_dir)
        .status();

    Ok(desktop_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_nxm() {
        let link = parse_nxm_url("nxm://skyrimspecialedition/mods/123/files/456").unwrap();
        assert_eq!(link.game_domain, "skyrimspecialedition");
        assert_eq!(link.mod_id, 123);
        assert_eq!(link.file_id, 456);
        assert!(link.key.is_none());
    }

    #[test]
    fn parses_with_query() {
        let link =
            parse_nxm_url("nxm://skyrim/mods/1/files/2?key=abc&expires=999").unwrap();
        assert_eq!(link.key.as_deref(), Some("abc"));
        assert_eq!(link.expires.as_deref(), Some("999"));
    }

    #[test]
    fn rejects_http() {
        assert!(parse_nxm_url("https://nexusmods.com/foo").is_err());
    }
}
