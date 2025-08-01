#![allow(clippy::too_many_arguments)]
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use cargo::{
    core::{
        compiler::{BuildConfig, Compilation, CompileMode, MessageFormat},
        registry::PackageRegistry,
        Package, PackageId, SourceId, Workspace,
    },
    ops::{CompileOptions, Packages},
    sources::{config::SourceConfigMap, RegistrySource},
    util::{cache_lock::CacheLockMode, context::GlobalContext, interning::InternedString},
};
use ouroboros::self_referencing;

struct OtherOptions {
    message_format: MessageFormat,
    manifest_path: Option<PathBuf>,
    build_tests: bool,
    needs_full_rebuild: bool,
    build_twizzler: bool,
}

use crate::{
    triple::{valid_targets, Arch, Triple},
    BuildOptions, CheckOptions, DocOptions, Profile,
};

fn locate_packages<'a>(workspace: &'a Workspace, kind: Option<&str>) -> Vec<Package> {
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
        .cloned()
        .collect()
}

fn get_cli_configs(
    build_config: crate::BuildConfig,
    _other_options: &OtherOptions,
) -> anyhow::Result<Vec<String>> {
    // in the future from the cli we might want enable arbitrary --cfg options

    // bring trait with write_fmt for write! to be used in scope
    use std::fmt::Write;

    // the currently supported build target specs
    // have a value of "unknown" for the machine, but
    // we specify the machine for conditional compilation
    // in the kernel via xtask cli
    let triple = Triple::new(
        build_config.arch,
        crate::triple::Machine::Unknown,
        crate::triple::Host::None,
        None,
    );
    let target_machine = build_config.machine.to_string();

    // start building the config
    let mut configs = format!(r#"target.{}.rustflags=["#, triple.to_string());

    // add in definition for machine target
    write!(configs, r#""--cfg=machine=\"{}\"""#, target_machine)?;

    // finish the cfg string
    write!(configs, "]")?;

    Ok(vec![configs])
}

fn build_third_party<'a>(
    user_workspace: &'a Workspace,
    mode: CompileMode,
    build_config: &crate::BuildConfig,
    other_options: &OtherOptions,
) -> anyhow::Result<Vec<Compilation<'a>>> {
    if !other_options.build_twizzler {
        return Ok(vec![]);
    }
    crate::toolchain::set_static();
    crate::toolchain::set_cc(&build_config.twz_triple());
    let config = user_workspace.gctx();
    let smap = SourceConfigMap::new(config)?;
    let mut registry = PackageRegistry::new_with_source_config(config, smap)?;
    let _g = config.acquire_package_cache_lock(CacheLockMode::MutateExclusive)?;
    let meta = user_workspace
        .custom_metadata()
        .expect("no third-party specification in Cargo.toml")
        .get("third-party")
        .expect("no third-party specification in Cargo.toml");

    if meta.as_table().unwrap().is_empty() {
        return Ok(vec![]);
    }
    crate::print_status_line("collection: third-party", Some(build_config));
    let ids: Vec<PackageId> = meta
        .as_table()
        .unwrap()
        .iter()
        .map(|item| {
            PackageId::try_new(
                item.0,
                item.1.as_str().unwrap(),
                SourceId::crates_io(config).unwrap(),
            )
            .unwrap()
        })
        .collect();

    registry
        .add_sources(Some(SourceId::crates_io(config).unwrap()))
        .unwrap();
    let rs = RegistrySource::remote(
        SourceId::crates_io(config).unwrap(),
        &Default::default(),
        config,
    )
    .unwrap();

    let ps = registry.get(&ids).unwrap();
    ps.sources_mut().insert(Box::new(rs));
    let packs = ps.get_many(ids.iter().cloned()).unwrap();

    let triple = Triple::new(
        build_config.arch,
        build_config.machine,
        crate::triple::Host::Twizzler,
        None,
    );
    let mut options = CompileOptions::new(config, mode)?;
    options.build_config = BuildConfig::new(config, None, false, &[triple.to_string()], mode)?;
    options.build_config.message_format = other_options.message_format;
    if build_config.profile == Profile::Release {
        options.build_config.requested_profile = InternedString::new("release");
    }
    options.build_config.force_rebuild = other_options.needs_full_rebuild;

    packs
        .into_iter()
        .cloned()
        .map(|item| {
            options.spec = Packages::Packages(vec![item.name().to_string()]);
            let ws =
                Workspace::ephemeral(item, config, config.target_dir().unwrap(), false).unwrap();
            cargo::ops::compile(&ws, &options)
        })
        .collect()
}

fn build_tools<'a>(
    workspace: &'a Workspace,
    mode: CompileMode,
    other_options: &OtherOptions,
) -> anyhow::Result<Compilation<'a>> {
    crate::toolchain::clear_cc();
    std::env::set_var("CARGO_TARGET_DIR", "target/tools");
    crate::print_status_line("collection: tools", None);
    let tools = locate_packages(workspace, Some("tool"));
    let mut options = CompileOptions::new(workspace.gctx(), mode)?;
    options.spec = Packages::Packages(tools.iter().map(|p| p.name().to_string()).collect());
    options.build_config.requested_profile = InternedString::new("release");
    options.build_config.message_format = other_options.message_format;
    cargo::ops::compile(workspace, &options)
}

fn build_static<'a>(
    workspace: &'a Workspace,
    mode: CompileMode,
    build_config: &crate::BuildConfig,
    other_options: &OtherOptions,
) -> anyhow::Result<Option<Compilation<'a>>> {
    if !other_options.build_twizzler {
        return Ok(None);
    }
    crate::toolchain::set_static();
    crate::toolchain::set_cc(&build_config.twz_triple());
    crate::print_status_line("collection: userspace-static", Some(build_config));
    // the currently supported build target triples
    // have a value of "unknown" for the machine, but
    // we might specify a different value for machine
    // on the cli for conditional compilation
    let triple = Triple::new(
        build_config.arch,
        crate::triple::Machine::Unknown,
        crate::triple::Host::Twizzler,
        None,
    );
    let packages = locate_packages(workspace, Some("static"));
    let mut options = CompileOptions::new(workspace.gctx(), mode)?;
    options.build_config =
        BuildConfig::new(workspace.gctx(), None, false, &[triple.to_string()], mode)?;
    options.build_config.message_format = other_options.message_format;
    if build_config.profile == Profile::Release {
        options.build_config.requested_profile = InternedString::new("release");
    }
    options.spec = Packages::Packages(packages.iter().map(|p| p.name().to_string()).collect());
    options.build_config.force_rebuild = other_options.needs_full_rebuild;
    Ok(Some(cargo::ops::compile(workspace, &options)?))
}

fn build_twizzler<'a>(
    workspace: &'a Workspace,
    mode: CompileMode,
    build_config: &crate::BuildConfig,
    other_options: &OtherOptions,
) -> anyhow::Result<Option<Compilation<'a>>> {
    if !other_options.build_twizzler {
        return Ok(None);
    }
    crate::toolchain::set_dynamic(&build_config.twz_triple());
    crate::toolchain::set_cc(&build_config.twz_triple());
    crate::print_status_line("collection: userspace", Some(build_config));
    // let triple =  build_config.twz_triple();
    // the currently supported build target triples
    // have a value of "unknown" for the machine, but
    // we might specify a different value for machine
    // on the cli for conditional compilation
    let triple = Triple::new(
        build_config.arch,
        crate::triple::Machine::Unknown,
        crate::triple::Host::Twizzler,
        None,
    );
    let packages = locate_packages(workspace, None);
    let mut options = CompileOptions::new(workspace.gctx(), mode)?;
    options.build_config =
        BuildConfig::new(workspace.gctx(), None, false, &[triple.to_string()], mode)?;
    options.build_config.message_format = other_options.message_format;
    if build_config.profile == Profile::Release {
        options.build_config.requested_profile = InternedString::new("release");
    }
    // TODO: the debug hook is currently only supported on x86_64.
    options.spec = Packages::Packages(
        packages
            .iter()
            .map(|p| p.name().to_string())
            .filter(|p| match p.as_str() {
                "debug" => {
                    if build_config.arch != Arch::X86_64 {
                        false
                    } else {
                        true
                    }
                }
                _ => true,
            })
            .collect(),
    );
    options.build_config.force_rebuild = other_options.needs_full_rebuild;
    Ok(Some(cargo::ops::compile(workspace, &options)?))
}

fn maybe_build_tests_dynamic<'a>(
    workspace: &'a Workspace,
    build_config: &crate::BuildConfig,
    _static_compilation: &Option<Compilation<'a>>,
    other_options: &OtherOptions,
) -> anyhow::Result<Option<Compilation<'a>>> {
    let mode = CompileMode::Test;
    if !other_options.build_tests || !other_options.build_twizzler {
        return Ok(None);
    }
    crate::toolchain::set_dynamic(&build_config.twz_triple());
    crate::toolchain::set_cc(&build_config.twz_triple());
    crate::print_status_line("collection: userspace::tests", Some(build_config));
    let triple = Triple::new(
        build_config.arch,
        build_config.machine,
        crate::triple::Host::Twizzler,
        None,
    );
    let packages = locate_packages(workspace, None);
    let mut options = CompileOptions::new(workspace.gctx(), mode)?;
    options.build_config =
        BuildConfig::new(workspace.gctx(), None, false, &[triple.to_string()], mode)?;
    options.build_config.message_format = other_options.message_format;
    if build_config.profile == Profile::Release {
        options.build_config.requested_profile = InternedString::new("release");
    }
    // Skip these. The macros don't compile right, here, and the others are low level and also don't
    // compile right. Some are deprecated and pending removal with the new twizzler crate.
    options.spec = Packages::Packages(
        packages
            .iter()
            .filter_map(|p| match p.name().as_str() {
                "twizzler-kernel-macros" => None,
                "twz-rt" => None,
                "monitor" => None,
                "monitor-api" => None,
                "bootstrap" => None,
                "secgate-macros" => None,
                "layout-derive" => None,
                "twizzler-object" => None,
                "twizzler-nando" => None,
                "twizzler-net" => None,
                "twizzler-futures" => None,
                "twizzler-async" => None,
                "twizzler-derive" => None,
                "tiny_http" => None,
                "test-tiny-http" => None,
                "object-store" => None,
                "montest-lib" => None,
                "naming" => None,
                //"naming-core" => None,
                "naming-srv" => None,
                "pager-srv" => None,
                "montest" => None,
                "uuhelper" => None,
                "unittest" => None,
                "unittest-report" => None,
                _ => Some(p.name().to_string()),
            })
            .collect(),
    );
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
    // The kernel config.toml sets its own rustflags.
    crate::toolchain::clear_rustflags();
    std::env::set_var("CARGO_TARGET_DIR", "target/kernel");
    crate::print_status_line("collection: kernel::tests", Some(build_config));
    let packages = locate_packages(workspace, Some("kernel"));
    let mut options = CompileOptions::new(workspace.gctx(), mode)?;
    if !build_config.is_default_target() {
        // the currently supported build target specs
        // have a value of "unknown" for the machine, but
        // we specify the machine for conditional compilation
        // in the kernel via xtask cli
        let triple = Triple::new(
            build_config.arch,
            crate::triple::Machine::Unknown,
            crate::triple::Host::None,
            None,
        );

        let mut target_spec = triple.to_string();
        target_spec.insert_str(0, "src/kernel/target-spec/");
        target_spec.push_str(".json");

        let bc = BuildConfig::new(workspace.gctx(), None, false, &[target_spec], mode)?;

        options.build_config = bc;
    }
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
    // The kernel config.toml sets its own rustflags.
    crate::toolchain::clear_rustflags();
    std::env::set_var("CARGO_TARGET_DIR", "target/kernel");
    crate::print_status_line("collection: kernel", Some(build_config));
    let packages = locate_packages(workspace, Some("kernel"));
    let mut options = CompileOptions::new(workspace.gctx(), mode)?;

    if !build_config.is_default_target() {
        // the currently supported build target specs
        // have a value of "unknown" for the machine, but
        // we specify the machine for conditional compilation
        // in the kernel via xtask cli
        let triple = Triple::new(
            build_config.arch,
            crate::triple::Machine::Unknown,
            crate::triple::Host::None,
            None,
        );

        let mut target_spec = triple.to_string();
        target_spec.insert_str(0, "src/kernel/target-spec/");
        target_spec.push_str(".json");

        let bc = BuildConfig::new(workspace.gctx(), None, false, &[target_spec], mode)?;

        options.build_config = bc;
    }

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
    #[allow(dead_code)]
    pub static_config: GlobalContext,
    #[allow(dead_code)]
    pub user_config: GlobalContext,
    #[allow(dead_code)]
    pub tools_config: GlobalContext,
    #[allow(dead_code)]
    pub kernel_config: GlobalContext,

    #[borrows(static_config)]
    #[covariant]
    pub static_workspace: Workspace<'this>,
    #[borrows(user_config)]
    #[covariant]
    pub user_workspace: Workspace<'this>,
    #[borrows(tools_config)]
    #[covariant]
    pub tools_workspace: Workspace<'this>,
    #[borrows(kernel_config)]
    #[covariant]
    #[allow(dead_code)]
    pub kernel_workspace: Workspace<'this>,

    #[borrows(tools_workspace)]
    #[covariant]
    pub tools_compilation: Compilation<'this>,
    #[borrows(kernel_workspace)]
    #[covariant]
    pub kernel_compilation: Compilation<'this>,
    #[borrows(static_workspace)]
    #[covariant]
    pub static_compilation: Option<Compilation<'this>>,
    #[borrows(user_workspace)]
    #[covariant]
    pub user_compilation: Option<Compilation<'this>>,
    #[borrows(user_workspace, user_compilation)]
    #[covariant]
    pub user_test_compilation: Option<Compilation<'this>>,
    #[borrows(kernel_workspace)]
    #[covariant]
    pub test_kernel_compilation: Option<Compilation<'this>>,
    #[borrows(user_workspace)]
    #[covariant]
    pub third_party_compilation: Vec<Compilation<'this>>,
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

fn check_build_target(config: crate::BuildConfig) -> anyhow::Result<()> {
    if valid_targets().contains(&(config.arch, config.machine)) {
        Ok(())
    } else {
        anyhow::bail!("build target is invalid");
    }
}

fn compile(
    bc: crate::BuildConfig,
    mode: CompileMode,
    other_options: &OtherOptions,
) -> anyhow::Result<TwizzlerCompilation> {
    check_build_target(bc)?;
    crate::toolchain::init_for_build(
        mode.is_doc() || mode.is_check() || !other_options.build_twizzler,
    )?;

    std::env::set_var("CARGO_TARGET_DIR", "target/tools");
    let mut tools_config = GlobalContext::default()?;
    tools_config.configure(0, false, None, false, false, false, &None, &[], &[])?;

    crate::toolchain::set_cc(&bc.twz_triple());
    crate::toolchain::set_dynamic(&bc.twz_triple());

    let mut config = GlobalContext::default()?;
    config.configure(0, false, None, false, false, false, &None, &[], &[])?;

    crate::toolchain::set_static();
    let mut static_config = GlobalContext::default()?;
    static_config.configure(0, false, None, false, false, false, &None, &[], &[])?;

    crate::toolchain::clear_rustflags();
    std::env::set_var("CARGO_TARGET_DIR", "target/kernel");
    let mut kernel_config = GlobalContext::default()?;
    // add in a feature flags to be used in the kernel
    let cli_config = get_cli_configs(bc, other_options).unwrap();
    kernel_config.configure(0, false, None, false, false, false, &None, &[], &cli_config)?;
    kernel_config.reload_rooted_at("src/kernel")?;

    let manifest_path = other_options
        .manifest_path
        .as_ref()
        .unwrap_or(&PathBuf::from("Cargo.toml"))
        .clone()
        .canonicalize()?;

    TwizzlerCompilation::try_new::<anyhow::Error>(
        static_config,
        config,
        tools_config,
        kernel_config,
        |c| Workspace::new(&manifest_path, c),
        |c| Workspace::new(&manifest_path, c),
        |c| Workspace::new(&manifest_path, c),
        |c| Workspace::new(&manifest_path, c),
        |w| build_tools(w, mode, other_options),
        |w| build_kernel(w, mode, &bc, other_options),
        |w| build_static(w, mode, &bc, other_options),
        |w| build_twizzler(w, mode, &bc, other_options),
        |w, uc| maybe_build_tests_dynamic(w, &bc, uc, other_options),
        |w| maybe_build_kernel_tests(w, &bc, other_options),
        |w| build_third_party(w, mode, &bc, other_options),
    )
}

pub(crate) fn do_docs(cli: DocOptions) -> anyhow::Result<TwizzlerCompilation> {
    let other_options = OtherOptions {
        message_format: MessageFormat::Human,
        manifest_path: None,
        build_tests: false,
        needs_full_rebuild: false,
        build_twizzler: true,
    };
    compile(
        cli.config,
        CompileMode::Doc {
            deps: false,
            json: false,
        },
        &other_options,
    )
}

pub(crate) fn do_build(cli: BuildOptions) -> anyhow::Result<TwizzlerCompilation> {
    let other_options = OtherOptions {
        message_format: MessageFormat::Human,
        manifest_path: None,
        build_tests: cli.tests,
        needs_full_rebuild: false,
        build_twizzler: !cli.kernel,
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
            crate::MessageFormat::JsonDiagnosticShort => MessageFormat::Json {
                render_diagnostics: false,
                short: true,
                ansi: false,
            },
            crate::MessageFormat::JsonDiagnosticRenderedAnsi => MessageFormat::Json {
                render_diagnostics: false,
                short: false,
                ansi: true,
            },
            crate::MessageFormat::JsonRenderDiagnostics => MessageFormat::Json {
                render_diagnostics: true,
                short: false,
                ansi: false,
            },
        },
        manifest_path: cli.manifest_path,
        build_tests: false,
        needs_full_rebuild: false,
        build_twizzler: !cli.kernel,
    };
    compile(
        cli.config,
        CompileMode::Check { test: false },
        &other_options,
    )?;
    Ok(())
}
