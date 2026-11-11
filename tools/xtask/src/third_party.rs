use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use anyhow::{anyhow, bail, Context};
use cargo::core::Workspace;
use cargo_toml::Value;

/// A `[workspace.metadata.third-party]` entry whose path holds this file instead of a
/// `Cargo.toml` is a bundle: the file lists the packages, initrd entries and disk files, with
/// paths relative to the bundle directory.
pub const BUNDLE_MANIFEST: &str = "twizzler-third-party.toml";

pub enum Source {
    /// Relative to the repo root.
    Path(PathBuf),
    Version(String),
}

pub struct DiskFile {
    /// Relative to the repo root.
    pub from: PathBuf,
    pub to: String,
    pub optional: bool,
}

impl DiskFile {
    /// One `disk-files` entry, its `from` relative to `base`.
    pub fn parse(item: &Value, base: &Path) -> anyhow::Result<Self> {
        let field = |key| {
            item.get(key)
                .and_then(|v| v.as_str())
                .with_context(|| format!("disk-files entry needs a string `{}`", key))
        };
        Ok(DiskFile {
            from: base.join(field("from")?),
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
    if let Some(list) = manifest.get("disk-files") {
        for item in list.as_array().context("disk-files must be an array")? {
            tp.disk_files.push(DiskFile::parse(item, dir)?);
        }
    }
    Ok(())
}
