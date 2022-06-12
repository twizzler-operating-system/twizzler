#![allow(clippy::too_many_arguments)]

use std::path::{Path, PathBuf};

use cargo::{
    core::{
        compiler::{BuildConfig, Compilation, CompileMode, MessageFormat},
        Package, Workspace,
    },
    ops::{CompileOptions, Packages},
    util::interning::InternedString,
    Config,
};
use ouroboros::self_referencing;

struct OtherOptions {
    message_format: MessageFormat,
    manifest_path: Option<PathBuf>,
    build_tests: bool,
    needs_full_rebuild: bool,
}

use crate::{triple::Triple, BuildOptions, CheckOptions, DocOptions, Profile};

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

fn build_tools<'a>(
    workspace: &'a Workspace,
    mode: CompileMode,
    other_options: &OtherOptions,
) -> anyhow::Result<Compilation<'a>> {
    crate::print_status_line("collection: tools", None);
    let tools = locate_packages(workspace, Some("tool"));
    let mut options = CompileOptions::new(workspace.config(), mode)?;
    options.spec = Packages::Packages(tools.iter().map(|p| p.name().to_string()).collect());
    options.build_config.requested_profile = InternedString::new("release");
    options.build_config.message_format = other_options.message_format;
    cargo::ops::compile(workspace, &options)
}

fn build_twizzler<'a>(
    workspace: &'a Workspace,
    mode: CompileMode,
    build_config: &crate::BuildConfig,
    other_options: &OtherOptions,
) -> anyhow::Result<Compilation<'a>> {
    crate::print_status_line("collection: userspace", Some(build_config));
    let triple = Triple::new(
        build_config.arch,
        build_config.machine,
        crate::triple::Host::Twizzler,
    );
    let packages = locate_packages(workspace, None);
    let mut options = CompileOptions::new(workspace.config(), mode)?;
    options.build_config = BuildConfig::new(workspace.config(), None, &[triple.to_string()], mode)?;
    options.build_config.message_format = other_options.message_format;
    if build_config.profile == Profile::Release {
        options.build_config.requested_profile = InternedString::new("release");
    }
    options.spec = Packages::Packages(packages.iter().map(|p| p.name().to_string()).collect());
    options.build_config.force_rebuild = other_options.needs_full_rebuild;
    cargo::ops::compile(workspace, &options)
}

fn maybe_build_tests<'a>(
    workspace: &'a Workspace,
    build_config: &crate::BuildConfig,
    other_options: &OtherOptions,
) -> anyhow::Result<Option<Compilation<'a>>> {
    let mode = CompileMode::Test;
    if !other_options.build_tests {
        return Ok(None);
    }
    crate::print_status_line("collection: userspace::tests", Some(build_config));
    let triple = Triple::new(
        build_config.arch,
        build_config.machine,
        crate::triple::Host::Twizzler,
    );
    let packages = locate_packages(workspace, None);
    let mut options = CompileOptions::new(workspace.config(), mode)?;
    options.build_config = BuildConfig::new(workspace.config(), None, &[triple.to_string()], mode)?;
    options.build_config.message_format = other_options.message_format;
    if build_config.profile == Profile::Release {
        options.build_config.requested_profile = InternedString::new("release");
    }
    options.spec = Packages::Packages(packages.iter().map(|p| p.name().to_string()).collect());
    options.build_config.force_rebuild = other_options.needs_full_rebuild;
    Ok(Some(cargo::ops::compile(workspace, &options)?))
}

fn maybe_build_kernel_tests<'a>(
    workspace: &'a Workspace,
    build_config: &crate::BuildConfig,
    other_options: &OtherOptions,
) -> anyhow::Result<Option<Compilation<'a>>> {
    let mode = CompileMode::Test;
    if !other_options.build_tests {
        return Ok(None);
    }
    crate::print_status_line("collection: kernel::tests", Some(build_config));
    let packages = locate_packages(workspace, Some("kernel"));
    let mut options = CompileOptions::new(workspace.config(), mode)?;
    options.build_config.message_format = other_options.message_format;
    if build_config.profile == Profile::Release {
        options.build_config.requested_profile = InternedString::new("release");
    }
    options.spec = Packages::Packages(packages.iter().map(|p| p.name().to_string()).collect());
    options.build_config.force_rebuild = other_options.needs_full_rebuild;
    Ok(Some(cargo::ops::compile(workspace, &options)?))
}

fn build_kernel<'a>(
    workspace: &'a Workspace,
    mode: CompileMode,
    build_config: &crate::BuildConfig,
    other_options: &OtherOptions,
) -> anyhow::Result<Compilation<'a>> {
    crate::print_status_line("collection: kernel", Some(build_config));
    let packages = locate_packages(workspace, Some("kernel"));
    let mut options = CompileOptions::new(workspace.config(), mode)?;
    options.build_config.message_format = other_options.message_format;
    if build_config.profile == Profile::Release {
        options.build_config.requested_profile = InternedString::new("release");
    }
    options.spec = Packages::Packages(packages.iter().map(|p| p.name().to_string()).collect());
    options.build_config.force_rebuild = other_options.needs_full_rebuild;
    cargo::ops::compile(workspace, &options)
}

#[self_referencing]
pub(crate) struct TwizzlerCompilation {
    pub user_config: Config,
    #[borrows(user_config)]
    #[covariant]
    pub user_workspace: Workspace<'this>,

    pub kernel_config: Config,
    #[borrows(kernel_config)]
    #[covariant]
    pub kernel_workspace: Workspace<'this>,

    #[borrows(user_workspace)]
    #[covariant]
    pub tools_compilation: Compilation<'this>,
    #[borrows(kernel_workspace)]
    #[covariant]
    pub kernel_compilation: Compilation<'this>,
    #[borrows(user_workspace)]
    #[covariant]
    pub user_compilation: Compilation<'this>,
    #[borrows(user_workspace)]
    #[covariant]
    pub test_compilation: Option<Compilation<'this>>,
    #[borrows(kernel_workspace)]
    #[covariant]
    pub test_kernel_compilation: Option<Compilation<'this>>,
}

impl TwizzlerCompilation {
    pub fn get_kernel_image(&self, tests: bool) -> &Path {
        if tests {
            &self
                .borrow_test_kernel_compilation()
                .as_ref()
                .expect("failed to get kernel test compilation when tests requested")
                .tests
                .get(0)
                .unwrap()
                .path
        } else {
            &self
                .borrow_kernel_compilation()
                .binaries
                .get(0)
                .unwrap()
                .path
        }
    }
}

fn compile(
    bc: crate::BuildConfig,
    mode: CompileMode,
    other_options: &OtherOptions,
) -> anyhow::Result<TwizzlerCompilation> {
    crate::toolchain::init_for_build(mode.is_doc() || mode.is_check())?;
    let mut config = Config::default()?;
    config.configure(0, false, None, false, false, false, &None, &[], &[])?;
    let mut kernel_config = Config::default()?;
    kernel_config.configure(0, false, None, false, false, false, &None, &[], &[])?;
    kernel_config.reload_rooted_at("src/kernel")?;
    let manifest_path = other_options
        .manifest_path
        .as_ref()
        .unwrap_or(&PathBuf::from("Cargo.toml"))
        .clone()
        .canonicalize()?;

    TwizzlerCompilation::try_new::<anyhow::Error>(
        config,
        |c| Workspace::new(&manifest_path, c),
        kernel_config,
        |c| Workspace::new(&manifest_path, c),
        |w| build_tools(w, mode, other_options),
        |w| build_kernel(w, mode, &bc, other_options),
        |w| build_twizzler(w, mode, &bc, other_options),
        |w| maybe_build_tests(w, &bc, other_options),
        |w| maybe_build_kernel_tests(w, &bc, other_options),
    )
}

pub(crate) fn do_docs(cli: DocOptions) -> anyhow::Result<TwizzlerCompilation> {
    let other_options = OtherOptions {
        message_format: MessageFormat::Human,
        manifest_path: None,
        build_tests: false,
        needs_full_rebuild: false,
    };
    compile(cli.config, CompileMode::Doc { deps: false }, &other_options)
}

pub(crate) fn do_build(cli: BuildOptions) -> anyhow::Result<TwizzlerCompilation> {
    let other_options = OtherOptions {
        message_format: MessageFormat::Human,
        manifest_path: None,
        build_tests: cli.tests,
        needs_full_rebuild: false,
    };
    compile(cli.config, CompileMode::Build, &other_options)
}

pub(crate) fn do_check(cli: CheckOptions) -> anyhow::Result<()> {
    let other_options = OtherOptions {
        message_format: match cli.message_format {
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
        manifest_path: cli.manifest_path,
        build_tests: false,
        needs_full_rebuild: false,
    };
    compile(cli.config, CompileMode::Check { test: false }, &other_options)?;
    Ok(())
}
