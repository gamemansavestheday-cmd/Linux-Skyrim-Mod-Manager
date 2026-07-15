//! FOMOD (`fomod/ModuleConfig.xml`) installer support.
//!
//! Covers the common path used by Nexus mods: install steps → option groups
//! with SelectAny/ExactlyOne/AtMostOne/AtLeastOne/All → file/folder copy
//! instructions. Conditional dependency logic is intentionally simplified —
//! flags and complex `<dependencies>` trees are ignored so a person can still
//! walk through the same choices MO2/Vortex present for the vast majority of
//! FOMODs.

use anyhow::{bail, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// How many options a person may pick in a group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupType {
    SelectAny,
    SelectAll,
    SelectExactlyOne,
    SelectAtMostOne,
    SelectAtLeastOne,
}

impl GroupType {
    fn parse(s: &str) -> Self {
        match s {
            "SelectAll" => Self::SelectAll,
            "SelectExactlyOne" => Self::SelectExactlyOne,
            "SelectAtMostOne" => Self::SelectAtMostOne,
            "SelectAtLeastOne" => Self::SelectAtLeastOne,
            _ => Self::SelectAny, // SelectAny + unknown → multi-pick
        }
    }
}

/// One selectable option inside a group.
#[derive(Debug, Clone)]
pub struct FomodOption {
    pub name: String,
    pub description: String,
    pub files: Vec<FileInstruction>,
}

/// A file or folder the option wants installed.
#[derive(Debug, Clone)]
pub struct FileInstruction {
    /// Path relative to the archive root (source).
    pub source: String,
    /// Destination under the mod content root (Data-relative). Empty means
    /// "same as source" for files, or the folder root for folders.
    pub destination: String,
    /// When true, `source` is a directory and should be copied recursively.
    pub is_folder: bool,
}

#[derive(Debug, Clone)]
pub struct OptionGroup {
    pub name: String,
    pub group_type: GroupType,
    pub options: Vec<FomodOption>,
}

#[derive(Debug, Clone)]
pub struct InstallStep {
    pub name: String,
    pub groups: Vec<OptionGroup>,
}

/// Parsed ModuleConfig.xml.
#[derive(Debug, Clone)]
pub struct ModuleConfig {
    pub module_name: String,
    pub steps: Vec<InstallStep>,
    /// Required files installed regardless of choices.
    pub required_files: Vec<FileInstruction>,
}

/// Per-step: for each group in that step, the option indices the user picked.
pub type StepChoices = Vec<Vec<usize>>;

/// Resolved list of files/folders to copy.
#[derive(Debug, Clone, Default)]
pub struct InstallPlan {
    pub files: Vec<FileInstruction>,
}

/// Locate `fomod/ModuleConfig.xml` under an extracted archive (case-insensitive).
pub fn find_module_config(root: &Path) -> Option<PathBuf> {
    // Direct path first (most common).
    for candidate in [
        root.join("fomod").join("ModuleConfig.xml"),
        root.join("FOMOD").join("ModuleConfig.xml"),
        root.join("fomod").join("moduleconfig.xml"),
    ] {
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    // Walk a couple of levels — some archives wrap once: ModName/fomod/...
    for entry in walkdir::WalkDir::new(root)
        .max_depth(3)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
    {
        let name = entry.file_name().to_string_lossy().to_lowercase();
        if name == "moduleconfig.xml" {
            if let Some(parent) = entry.path().parent() {
                let parent_name = parent
                    .file_name()
                    .map(|s| s.to_string_lossy().to_lowercase())
                    .unwrap_or_default();
                if parent_name == "fomod" {
                    return Some(entry.path().to_path_buf());
                }
            }
        }
    }
    None
}

/// Parse ModuleConfig.xml text into a structured config.
pub fn parse_module_config(xml: &str) -> Result<ModuleConfig> {
    let doc = roxmltree::Document::parse(xml).context("parsing ModuleConfig.xml")?;
    let root = doc.root_element();

    let module_name = root
        .descendants()
        .find(|n| n.has_tag_name("moduleName"))
        .and_then(|n| n.text())
        .unwrap_or("")
        .trim()
        .to_string();

    let mut required_files = Vec::new();
    if let Some(req) = root.descendants().find(|n| n.has_tag_name("requiredInstallFiles")) {
        required_files.extend(parse_file_list(&req));
    }

    let mut steps = Vec::new();
    for step_node in root
        .descendants()
        .filter(|n| n.has_tag_name("installStep"))
    {
        let step_name = step_node
            .attribute("name")
            .unwrap_or("Install Step")
            .to_string();
        let mut groups = Vec::new();
        for group_node in step_node
            .descendants()
            .filter(|n| n.has_tag_name("group"))
        {
            // Only direct-ish groups under this step's optionalFileGroups —
            // roxmltree descendants includes nested; filter by parent chain
            // would be ideal, but groups aren't nested in practice.
            let group_name = group_node
                .attribute("name")
                .unwrap_or("Options")
                .to_string();
            let group_type = GroupType::parse(group_node.attribute("type").unwrap_or("SelectAny"));
            let mut options = Vec::new();
            for plugin in group_node.children().filter(|n| n.has_tag_name("plugins")).flat_map(|p| {
                p.children()
                    .filter(|n| n.has_tag_name("plugin"))
                    .collect::<Vec<_>>()
            }) {
                let name = plugin.attribute("name").unwrap_or("Option").to_string();
                let description = plugin
                    .children()
                    .find(|n| n.has_tag_name("description"))
                    .and_then(|n| n.text())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let mut files = Vec::new();
                if let Some(files_node) = plugin.children().find(|n| n.has_tag_name("files")) {
                    files.extend(parse_file_list(&files_node));
                }
                options.push(FomodOption {
                    name,
                    description,
                    files,
                });
            }
            // Some FOMODs put <plugin> under <plugins> with different nesting;
            // also try descendants one level if empty.
            if options.is_empty() {
                for plugin in group_node.descendants().filter(|n| n.has_tag_name("plugin")) {
                    // Skip nested plugins that belong to a different group.
                    if plugin
                        .ancestors()
                        .any(|a| a.has_tag_name("group") && a != group_node)
                    {
                        // plugin's nearest group ancestor should be us
                        let nearest_group = plugin
                            .ancestors()
                            .find(|a| a.has_tag_name("group"));
                        if nearest_group != Some(group_node) {
                            continue;
                        }
                    }
                    let name = plugin.attribute("name").unwrap_or("Option").to_string();
                    if options.iter().any(|o| o.name == name) {
                        continue;
                    }
                    let description = plugin
                        .children()
                        .find(|n| n.has_tag_name("description"))
                        .and_then(|n| n.text())
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    let mut files = Vec::new();
                    if let Some(files_node) = plugin.children().find(|n| n.has_tag_name("files")) {
                        files.extend(parse_file_list(&files_node));
                    }
                    options.push(FomodOption {
                        name,
                        description,
                        files,
                    });
                }
            }
            groups.push(OptionGroup {
                name: group_name,
                group_type,
                options,
            });
        }
        steps.push(InstallStep {
            name: step_name,
            groups,
        });
    }

    // ConditionalFileInstalls: include all pattern file lists unconditionally
    // when there are no steps with options (degraded but useful); skip complex
    // deps for now so we don't install everything from every pattern.

    Ok(ModuleConfig {
        module_name,
        steps,
        required_files,
    })
}

fn parse_file_list(node: &roxmltree::Node<'_, '_>) -> Vec<FileInstruction> {
    let mut out = Vec::new();
    for child in node.children() {
        let is_folder = child.has_tag_name("folder");
        let is_file = child.has_tag_name("file");
        if !is_folder && !is_file {
            continue;
        }
        let source = child.attribute("source").unwrap_or("").to_string();
        if source.is_empty() {
            continue;
        }
        let destination = child
            .attribute("destination")
            .unwrap_or("")
            .to_string();
        out.push(FileInstruction {
            source,
            destination,
            is_folder,
        });
    }
    out
}

/// Build the install plan from user choices. `choices[step_idx][group_idx]` is
/// the list of selected option indices for that group.
pub fn resolve_install_plan(
    cfg: &ModuleConfig,
    choices: &[StepChoices],
) -> Result<InstallPlan> {
    let mut plan = InstallPlan {
        files: cfg.required_files.clone(),
    };
    if choices.len() != cfg.steps.len() {
        bail!(
            "choice count ({}) does not match step count ({})",
            choices.len(),
            cfg.steps.len()
        );
    }
    for (step_idx, step) in cfg.steps.iter().enumerate() {
        let step_choices = &choices[step_idx];
        if step_choices.len() != step.groups.len() {
            bail!(
                "step '{name}' expected {expected} group choice(s), got {got}",
                name = step.name,
                expected = step.groups.len(),
                got = step_choices.len()
            );
        }
        for (group_idx, group) in step.groups.iter().enumerate() {
            for &opt_idx in &step_choices[group_idx] {
                let option = group
                    .options
                    .get(opt_idx)
                    .with_context(|| {
                        format!(
                            "option index {opt_idx} out of range in group '{}'",
                            group.name
                        )
                    })?;
                plan.files.extend(option.files.clone());
            }
        }
    }
    Ok(plan)
}

/// Copy every planned file/folder from `archive_root` into `content_dir`.
/// Returns the number of files written.
pub fn apply_install_plan(
    archive_root: &Path,
    content_dir: &Path,
    plan: &InstallPlan,
) -> Result<usize> {
    fs::create_dir_all(content_dir)
        .with_context(|| format!("creating content dir {}", content_dir.display()))?;
    let mut count = 0usize;
    for instr in &plan.files {
        let src = archive_root.join(normalize_rel(&instr.source));
        if !src.exists() {
            // FOMOD sources sometimes use backslashes or different casing.
            let alt = archive_root.join(instr.source.replace('\\', "/"));
            if !alt.exists() {
                // Skip missing sources rather than aborting the whole install —
                // some options list optional assets that aren't always packed.
                continue;
            }
            count += copy_instruction(&alt, content_dir, instr)?;
            continue;
        }
        count += copy_instruction(&src, content_dir, instr)?;
    }
    // Normalize Data wrappers after FOMOD copy.
    crate::store::normalize_root_public(content_dir)?;
    Ok(count)
}

fn copy_instruction(src: &Path, content_dir: &Path, instr: &FileInstruction) -> Result<usize> {
    let mut count = 0usize;
    if instr.is_folder || src.is_dir() {
        let dest_rel = if instr.destination.is_empty() {
            PathBuf::new()
        } else {
            PathBuf::from(normalize_rel(&instr.destination))
        };
        let dest_root = content_dir.join(&dest_rel);
        for entry in walkdir::WalkDir::new(src)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
        {
            let rel = entry.path().strip_prefix(src).unwrap_or(entry.path());
            let target = dest_root.join(rel);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(entry.path(), &target).with_context(|| {
                format!(
                    "copying FOMOD {} -> {}",
                    entry.path().display(),
                    target.display()
                )
            })?;
            count += 1;
        }
    } else {
        let dest_rel = if instr.destination.is_empty() {
            PathBuf::from(
                src.file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| "file".into()),
            )
        } else {
            let d = normalize_rel(&instr.destination);
            // destination may be a directory path or a full file path
            let dest_path = PathBuf::from(&d);
            if d.ends_with('/') || d.ends_with('\\') {
                dest_path.join(src.file_name().unwrap_or_default())
            } else if dest_path.extension().is_none() && !src.extension().is_none() {
                // destination looks like a folder without trailing slash
                dest_path.join(src.file_name().unwrap_or_default())
            } else {
                dest_path
            }
        };
        let target = content_dir.join(dest_rel);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(src, &target)
            .with_context(|| format!("copying FOMOD {} -> {}", src.display(), target.display()))?;
        count += 1;
    }
    Ok(count)
}

fn normalize_rel(p: &str) -> String {
    p.replace('\\', "/")
        .trim_start_matches("./")
        .trim_start_matches('/')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
    <config>
      <moduleName>Test Mod</moduleName>
      <requiredInstallFiles>
        <file source="base.esp" destination="base.esp" />
      </requiredInstallFiles>
      <installSteps>
        <installStep name="Options">
          <optionalFileGroups>
            <group name="Pick" type="SelectExactlyOne">
              <plugins>
                <plugin name="A">
                  <description>Option A</description>
                  <files>
                    <file source="a.esp" destination="a.esp" />
                  </files>
                </plugin>
                <plugin name="B">
                  <description>Option B</description>
                  <files>
                    <folder source="b_folder" destination="" />
                  </files>
                </plugin>
              </plugins>
            </group>
          </optionalFileGroups>
        </installStep>
      </installSteps>
    </config>
    "#;

    #[test]
    fn parses_sample() {
        let cfg = parse_module_config(SAMPLE).unwrap();
        assert_eq!(cfg.module_name, "Test Mod");
        assert_eq!(cfg.required_files.len(), 1);
        assert_eq!(cfg.steps.len(), 1);
        assert_eq!(cfg.steps[0].groups[0].options.len(), 2);
        assert_eq!(
            cfg.steps[0].groups[0].group_type,
            GroupType::SelectExactlyOne
        );
    }

    #[test]
    fn resolve_picks_option() {
        let cfg = parse_module_config(SAMPLE).unwrap();
        let plan = resolve_install_plan(&cfg, &[vec![vec![0]]]).unwrap();
        // required + option A
        assert_eq!(plan.files.len(), 2);
        assert_eq!(plan.files[1].source, "a.esp");
    }

    #[test]
    fn find_config_in_tree() {
        let dir = std::env::temp_dir().join(format!("fomod-find-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let fomod = dir.join("fomod");
        fs::create_dir_all(&fomod).unwrap();
        fs::write(fomod.join("ModuleConfig.xml"), SAMPLE).unwrap();
        assert!(find_module_config(&dir).is_some());
        let _ = fs::remove_dir_all(&dir);
    }
}
