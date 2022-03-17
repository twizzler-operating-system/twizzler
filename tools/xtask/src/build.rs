use std::path::PathBuf;

use cargo::{
    core::{
        compiler::{BuildConfig, CompileMode, MessageFormat},
        Package, Workspace,
    },
    ops::{CompileOptions, Packages},
    util::interning::InternedString,
    Config,
};

struct OtherOptions {
    message_format: MessageFormat,
}

use crate::{triple::Triple, BuildOptions, CheckOptions, Profile};

fn locate_packages<'a>(workspace: &'a Workspace, kind: Option<&str>) -> Vec<&'a Package> {
    workspace
        .members()
        .filter(|p| {
            let meta = p.manifest().custom_metadata();
            if let Some(meta) = meta {
                let tb = meta.get("twizzler-build");
                if let (Some(mk), Some(kind)) = (tb, kind) {
                    if let Some(s) = mk.as_str() {
                        s == kind
                    } else {
                        false
                    }
                } else {
                    kind.is_none() == tb.is_none()
                }
            } else {
                kind.is_none()
            }
        })
        .collect()
}

fn build_tools(
    workspace: &Workspace,
    mode: CompileMode,
    other_options: &OtherOptions,
) -> anyhow::Result<()> {
    let tools = locate_packages(&workspace, Some("tool"));
    let mut options = CompileOptions::new(workspace.config(), mode)?;
    options.spec = Packages::Packages(tools.iter().map(|p| p.name().to_string()).collect());
    options.build_config.requested_profile = InternedString::new("release");
    options.build_config.message_format = other_options.message_format;
    cargo::ops::compile(workspace, &options)?;
    Ok(())
}

fn build_twizzler(
    workspace: &Workspace,
    mode: CompileMode,
    build_config: &crate::BuildConfig,
    other_options: &OtherOptions,
) -> anyhow::Result<()> {
    let triple = Triple::new(
        build_config.arch,
        build_config.machine,
        crate::triple::Host::Twizzler,
    );
    let tools = locate_packages(&workspace, None);
    let mut options = CompileOptions::new(workspace.config(), mode)?;
    options.build_config = BuildConfig::new(workspace.config(), None, &[triple.to_string()], mode)?;
    options.build_config.message_format = other_options.message_format;
    if build_config.profile == Profile::Release {
        options.build_config.requested_profile = InternedString::new("release");
    }
    options.spec = Packages::Packages(tools.iter().map(|p| p.name().to_string()).collect());
    cargo::ops::compile(workspace, &options)?;
    Ok(())
}

fn build_kernel(
    workspace: &Workspace,
    mode: CompileMode,
    build_config: &crate::BuildConfig,
    other_options: &OtherOptions,
) -> anyhow::Result<()> {
    let tools = locate_packages(&workspace, Some("kernel"));
    let mut options = CompileOptions::new(workspace.config(), mode)?;
    options.build_config.message_format = other_options.message_format;
    if build_config.profile == Profile::Release {
        options.build_config.requested_profile = InternedString::new("release");
    }
    options.spec = Packages::Packages(tools.iter().map(|p| p.name().to_string()).collect());
    cargo::ops::compile(workspace, &options)?;
    Ok(())
}

fn compile(
    bc: crate::BuildConfig,
    mode: CompileMode,
    other_options: OtherOptions,
) -> anyhow::Result<()> {
    crate::toolchain::init_for_build()?;
    let mut config = Config::default()?;
    config.configure(0, false, None, false, false, false, &None, &[], &[])?;
    let mut kernel_config = Config::default()?;
    kernel_config.configure(0, false, None, false, false, false, &None, &[], &[])?;
    kernel_config.reload_rooted_at("src/kernel")?;
    let workspace =
        cargo::core::Workspace::new(&PathBuf::from("Cargo.toml").canonicalize()?, &config)?;
    let kernel_workspace =
        cargo::core::Workspace::new(&PathBuf::from("Cargo.toml").canonicalize()?, &kernel_config)?;
    build_tools(&workspace, mode, &other_options)?;
    build_kernel(&kernel_workspace, mode, &bc, &other_options)?;
    build_twizzler(&workspace, mode, &bc, &other_options)?;
    Ok(())
}

pub(crate) fn do_build(cli: BuildOptions) -> anyhow::Result<()> {
    let other_options = OtherOptions {
        message_format: MessageFormat::Short,
    };
    compile(cli.config, CompileMode::Build, other_options)
}

pub(crate) fn do_check(cli: CheckOptions) -> anyhow::Result<()> {
    let other_options = OtherOptions {
        message_format: match cli.message_fmt {
            crate::MessageFormat::Human => MessageFormat::Human,
            crate::MessageFormat::Short => MessageFormat::Short,
            crate::MessageFormat::Json => MessageFormat::Json {
                render_diagnostics: false,
                short: false,
                ansi: false,
            },
            crate::MessageFormat::JsonDiagnosticShort => todo!(),
            crate::MessageFormat::JsonDiagnosticRenderedAnsi => todo!(),
            crate::MessageFormat::JsonRenderDiagnostics => todo!(),
        },
    };
    compile(cli.config, CompileMode::Build, other_options)
}
