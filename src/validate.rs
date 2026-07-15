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

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

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

/// Build a master-dependency graph for the given plugins. Edges point from
/// a plugin to each of its masters that are also in the enabled set (so
/// "A depends on B" means A → B and B should load before A).
fn master_edges(
    plugins: &[String],
    find_plugin_path: &impl Fn(&str) -> Option<PathBuf>,
) -> HashMap<String, Vec<String>> {
    let lower_to_canonical: HashMap<String, String> = plugins
        .iter()
        .map(|p| (p.to_lowercase(), p.clone()))
        .collect();
    let mut edges: HashMap<String, Vec<String>> = HashMap::new();
    for plugin in plugins {
        edges.entry(plugin.clone()).or_default();
        let Some(path) = find_plugin_path(plugin) else {
            continue;
        };
        for master in read_masters(&path) {
            if let Some(canonical) = lower_to_canonical.get(&master.to_lowercase()) {
                if !canonical.eq_ignore_ascii_case(plugin) {
                    edges
                        .entry(plugin.clone())
                        .or_default()
                        .push(canonical.clone());
                }
            }
        }
    }
    edges
}

/// Detect circular master dependencies among enabled plugins. Each cycle is
/// returned as a list of plugin names ending with the first name again so
/// the chain reads as a loop (e.g. `A -> B -> A`).
pub fn find_cycles(
    plugins: &[String],
    find_plugin_path: impl Fn(&str) -> Option<PathBuf>,
) -> Vec<Vec<String>> {
    let edges = master_edges(plugins, &find_plugin_path);
    let mut cycles = Vec::new();
    let mut global_visited: HashSet<String> = HashSet::new();

    for start in plugins {
        if global_visited.contains(&start.to_lowercase()) {
            continue;
        }
        let mut path: Vec<String> = Vec::new();
        let mut on_stack: HashSet<String> = HashSet::new();
        dfs_cycles(
            start,
            &edges,
            &mut path,
            &mut on_stack,
            &mut global_visited,
            &mut cycles,
        );
    }
    cycles
}

fn dfs_cycles(
    node: &str,
    edges: &HashMap<String, Vec<String>>,
    path: &mut Vec<String>,
    on_stack: &mut HashSet<String>,
    global_visited: &mut HashSet<String>,
    cycles: &mut Vec<Vec<String>>,
) {
    let key = node.to_lowercase();
    if on_stack.contains(&key) {
        if let Some(start) = path.iter().position(|p| p.eq_ignore_ascii_case(node)) {
            let mut cycle: Vec<String> = path[start..].to_vec();
            cycle.push(node.to_string());
            // Dedup by normalized signature.
            let sig: Vec<String> = cycle.iter().map(|s| s.to_lowercase()).collect();
            if !cycles.iter().any(|c| {
                c.iter().map(|s| s.to_lowercase()).collect::<Vec<_>>() == sig
            }) {
                cycles.push(cycle);
            }
        }
        return;
    }
    if global_visited.contains(&key) {
        return;
    }
    global_visited.insert(key.clone());
    on_stack.insert(key.clone());
    path.push(node.to_string());

    if let Some(deps) = edges.get(node) {
        for dep in deps {
            dfs_cycles(dep, edges, path, on_stack, global_visited, cycles);
        }
    } else {
        // Edge map keys are canonical names; also try case-insensitive match.
        for (k, deps) in edges {
            if k.eq_ignore_ascii_case(node) {
                for dep in deps {
                    dfs_cycles(dep, edges, path, on_stack, global_visited, cycles);
                }
                break;
            }
        }
    }

    path.pop();
    on_stack.remove(&key);
}

/// Topologically sort plugins so every master loads before the plugins that
/// require it. On cycles, returns `Err` with the same cycle lists as
/// [`find_cycles`]. Stable among independent plugins (preserves relative
/// order of the input where the graph does not constrain them).
pub fn sort_plugins(
    plugins: &[String],
    find_plugin_path: impl Fn(&str) -> Option<PathBuf>,
) -> Result<Vec<String>, Vec<Vec<String>>> {
    let cycles = find_cycles(plugins, &find_plugin_path);
    if !cycles.is_empty() {
        return Err(cycles);
    }

    // Edge: plugin → master means plugin depends on master.
    // For Kahn's algorithm we need indegree on "must come after" edges:
    // master must load before plugin ⇒ edge master → plugin.
    let dep_edges = master_edges(plugins, &find_plugin_path);
    let mut successors: HashMap<String, Vec<String>> = HashMap::new();
    let mut indegree: HashMap<String, usize> = HashMap::new();
    for p in plugins {
        indegree.entry(p.clone()).or_insert(0);
        successors.entry(p.clone()).or_default();
    }
    for (plugin, masters) in &dep_edges {
        for master in masters {
            successors
                .entry(master.clone())
                .or_default()
                .push(plugin.clone());
            *indegree.entry(plugin.clone()).or_insert(0) += 1;
        }
    }

    // Seed queue in original order so independent plugins keep relative order.
    let mut queue: VecDeque<String> = VecDeque::new();
    for p in plugins {
        if indegree.get(p).copied().unwrap_or(0) == 0 {
            queue.push_back(p.clone());
        }
    }

    let mut sorted = Vec::with_capacity(plugins.len());
    while let Some(node) = queue.pop_front() {
        sorted.push(node.clone());
        let nexts = successors.get(&node).cloned().unwrap_or_default();
        for next in nexts {
            if let Some(d) = indegree.get_mut(&next) {
                *d = d.saturating_sub(1);
                if *d == 0 {
                    queue.push_back(next);
                }
            }
        }
    }

    if sorted.len() != plugins.len() {
        // Should be unreachable if cycle detection worked, but fail safe.
        return Err(find_cycles(plugins, find_plugin_path));
    }
    Ok(sorted)
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
