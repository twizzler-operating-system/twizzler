use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use anyhow::{anyhow, bail, Context};
use cargo::core::Workspace;
use cargo_toml::Value;

/// A `[workspace.metadata.third-party]` entry whose path holds this file instead of a
/// `Cargo.toml` is a bundle: the file lists the packages, external builds, initrd entries and disk
/// files, with paths relative to the bundle directory.
pub const BUNDLE_MANIFEST: &str = "twizzler-third-party.toml";

pub enum Source {
    /// Relative to the repo root.
    Path(PathBuf),
    Version(String),
}

pub struct DiskFile {
    /// Relative to the repo root, or to the output directory of `build` when that is set.
    pub from: PathBuf,
    pub to: String,
    pub optional: bool,
    /// Name of the external build that produces this file.
    pub build: Option<String>,
}

/// A bundle `[[build]]` entry: something built by a command of the bundle's own rather than by
/// cargo. See `build::run_external_builds` for what the command is given.
pub struct ExternalBuild {
    pub name: String,
    /// The bundle directory, relative to the repo root. The command runs here.
    pub dir: PathBuf,
    pub command: Vec<String>,
    /// Relative to the repo root. The build is skipped while this path is absent.
    pub requires: Option<PathBuf>,
}

impl ExternalBuild {
    pub fn present(&self) -> bool {
        self.requires.as_ref().is_none_or(|p| p.exists())
    }

    /// Relative to the repo root.
    pub fn out_dir(&self, config: &crate::BuildConfig) -> PathBuf {
        Path::new("target/external")
            .join(config.twz_triple().to_string())
            .join(config.profile.to_string())
            .join(&self.name)
    }
}

impl DiskFile {
    /// One `disk-files` entry, its `from` relative to `base`.
    pub fn parse(item: &Value, base: &Path) -> anyhow::Result<Self> {
        let field = |key| {
            item.get(key)
                .and_then(|v| v.as_str())
                .with_context(|| format!("disk-files entry needs a string `{}`", key))
        };
        let build = item.get("build").and_then(|v| v.as_str());
        Ok(DiskFile {
            from: match build {
                Some(_) => PathBuf::from(field("from")?),
                None => base.join(field("from")?),
            },
            build: build.map(str::to_string),
            to: field("to")?.to_string(),
            optional: item
                .get("optional")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        })
    }
}

/// The root manifest's third-party table with bundles expanded.
#[derive(Default)]
pub struct ThirdParty {
    pub packages: BTreeMap<String, Source>,
    /// Bundle-declared package names to pack into the initrd.
    pub initrd: Vec<String>,
    /// Bundle-declared disk files.
    pub disk_files: Vec<DiskFile>,
    pub builds: Vec<ExternalBuild>,
}

pub fn third_party(workspace: &Workspace) -> anyhow::Result<&'static ThirdParty> {
    static CACHE: OnceLock<ThirdParty> = OnceLock::new();
    if let Some(tp) = CACHE.get() {
        return Ok(tp);
    }
    let tp = load(workspace)?;
    Ok(CACHE.get_or_init(|| tp))
}

fn load(workspace: &Workspace) -> anyhow::Result<ThirdParty> {
    let mut tp = ThirdParty::default();
    let Some(table) = workspace
        .custom_metadata()
        .and_then(|m| m.get("third-party"))
    else {
        return Ok(tp);
    };
    let table = table
        .as_table()
        .ok_or_else(|| anyhow!("third-party metadata must be a table"))?;
    for (name, spec) in table {
        let bundle = spec
            .get("path")
            .and_then(|p| p.as_str())
            .map(Path::new)
            .filter(|p| !p.join("Cargo.toml").is_file() && p.join(BUNDLE_MANIFEST).is_file());
        match bundle {
            Some(dir) => {
                load_bundle(&mut tp, dir).with_context(|| format!("third-party bundle {}", name))?
            }
            None => add_package(&mut tp, name, spec, Path::new(""))?,
        }
    }
    Ok(tp)
}

fn add_package(tp: &mut ThirdParty, name: &str, spec: &Value, base: &Path) -> anyhow::Result<()> {
    let source = if let Some(version) = spec.as_str() {
        Source::Version(version.to_string())
    } else if spec.is_table() {
        let path = spec
            .get("path")
            .and_then(|p| p.as_str())
            .ok_or_else(|| anyhow!("third-party package {}: missing string `path`", name))?;
        Source::Path(base.join(path))
    } else {
        bail!("third-party package {}: invalid entry", name);
    };
    if tp.packages.insert(name.to_string(), source).is_some() {
        bail!("third-party package {} is defined more than once", name);
    }
    Ok(())
}

fn load_bundle(tp: &mut ThirdParty, dir: &Path) -> anyhow::Result<()> {
    let manifest: Value = std::fs::read_to_string(dir.join(BUNDLE_MANIFEST))?.parse()?;
    let packages = manifest.get("packages").and_then(|p| p.as_table());
    for (name, spec) in packages.into_iter().flatten() {
        add_package(tp, name, spec, dir)?;
    }
    if let Some(list) = manifest.get("initrd") {
        for item in list.as_array().context("initrd must be an array")? {
            let name = item.as_str().context("initrd entry must be a string")?;
            if !packages.is_some_and(|p| p.contains_key(name)) {
                bail!("initrd entry {} is not in [packages]", name);
            }
            tp.initrd.push(name.to_string());
        }
    }
    if let Some(list) = manifest.get("build") {
        for item in list.as_array().context("build must be an array of tables")? {
            let name = item
                .get("name")
                .and_then(|v| v.as_str())
                .context("build entry needs a string `name`")?;
            if tp.builds.iter().any(|b| b.name == name) {
                bail!("external build {} is defined more than once", name);
            }
            let command: Vec<String> = item
                .get("command")
                .and_then(|v| v.as_array())
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect();
            if command.is_empty() {
                bail!("build {} needs a `command` array of strings", name);
            }
            tp.builds.push(ExternalBuild {
                name: name.to_string(),
                dir: dir.to_path_buf(),
                command,
                requires: item
                    .get("requires")
                    .and_then(|v| v.as_str())
                    .map(|p| dir.join(p)),
            });
        }
    }
    if let Some(list) = manifest.get("disk-files") {
        for item in list.as_array().context("disk-files must be an array")? {
            let file = DiskFile::parse(item, dir)?;
            if let Some(build) = &file.build {
                if !tp.builds.iter().any(|b| b.name == *build) {
                    bail!("disk-files entry names unknown build {}", build);
                }
            }
            tp.disk_files.push(file);
        }
    }
    Ok(())
}
