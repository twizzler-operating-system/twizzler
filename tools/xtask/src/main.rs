#![feature(explicit_generic_args_with_impl_trait)]

mod build;
mod image;
mod qemu;
mod toolchain;
mod triple;

use std::path::PathBuf;

use clap::{ArgEnum, Args, Parser, Subcommand};
use triple::{Arch, Machine};

#[derive(Parser, Debug)]
#[clap(name = "xtask", author = "Daniel Bittman <danielbittman1@gmail.com>", version = "1.0", about = "Build system for Twizzler", long_about = None)]
struct Cli {
    #[clap(subcommand)]
    command: Option<Commands>,
}

#[derive(Debug, ArgEnum, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Profile {
    Debug,
    Release,
}

impl ToString for Profile {
    fn to_string(&self) -> String {
        match self {
            Profile::Debug => "debug",
            Profile::Release => "release",
        }
        .to_string()
    }
}

impl Default for Profile {
    fn default() -> Self {
        Self::Debug
    }
}

#[derive(Args, Debug, Clone, Copy)]
struct BuildConfig {
    #[clap(short, long, arg_enum, default_value_t = Profile::Debug, help = "Select build profile.")]
    pub profile: Profile,
    #[clap(short, long, arg_enum, default_value_t = Arch::X86_64, help = "Select target architecture.")]
    pub arch: Arch,
    #[clap(short, long, arg_enum, default_value_t = Machine::Unknown, help = "Select target machine.")]
    pub machine: Machine,
}

impl BuildConfig {
    fn is_default_arch(&self) -> bool {
        self.arch == Arch::X86_64
    }

    fn is_default_machine(&self) -> bool {
        self.machine == Machine::Unknown
    }

    pub fn is_default_target(&self) -> bool {
        self.is_default_arch() && self.is_default_machine()
    }
}

#[derive(Args, Debug)]
struct BuildOptions {
    #[clap(flatten)]
    pub config: BuildConfig,
    #[clap(long, short, help = "Build tests-enabled system.")]
    tests: bool,
    #[clap(long, short, help = "Only build kernel part of system.")]
    kernel: bool,
}

#[derive(Args, Debug)]
struct DocOptions {
    #[clap(flatten)]
    pub config: BuildConfig,
}

#[derive(ArgEnum, Debug, Clone, Copy)]
enum MessageFormat {
    Human,
    Short,
    Json,
    JsonDiagnosticShort,
    JsonDiagnosticRenderedAnsi,
    JsonRenderDiagnostics,
}

#[derive(Args, Debug)]
struct CheckOptions {
    #[clap(flatten)]
    pub config: BuildConfig,
    #[clap(long, short)]
    pub manifest_path: Option<PathBuf>,
    #[clap(long, short, arg_enum, default_value_t = MessageFormat::Human)]
    pub message_format: MessageFormat,
    #[clap(long, short)]
    pub workspace: bool,
    #[clap(long, short, help = "Only build kernel part of system.")]
    kernel: bool,
}

#[derive(Args, Debug, Clone, Copy)]
struct ImageOptions {
    #[clap(flatten)]
    pub config: BuildConfig,
    #[clap(long, short, help = "Build tests-enabled system.")]
    tests: bool,
    #[clap(long, short, help = "Only build kernel part of system.")]
    kernel: bool,
}

impl From<ImageOptions> for BuildOptions {
    fn from(io: ImageOptions) -> Self {
        Self {
            config: io.config,
            tests: io.tests,
            kernel: io.kernel,
        }
    }
}

#[derive(Args, Debug, Clone)]
struct QemuOptions {
    #[clap(flatten)]
    config: BuildConfig,
    #[clap(
        long,
        short,
        help = "Additional options to pass to Qemu. May be specified multiple times."
    )]
    qemu_options: Vec<String>,
    #[clap(long, short, help = "Run tests instead of booting normally.")]
    tests: bool,
}

impl From<&QemuOptions> for ImageOptions {
    fn from(qo: &QemuOptions) -> Self {
        Self {
            config: qo.config,
            tests: qo.tests,
            kernel: false,
        }
    }
}

#[derive(clap::Args, Debug)]
struct BootstrapOptions {
    #[clap(
        long,
        help = "Skip updating git submodules before bootstrapping the toolchain."
    )]
    skip_submodules: bool,
    #[clap(
        long,
        help = "Don't remove the target/ directory after rebuilding the toolchain."
    )]
    keep_old_artifacts: bool,
}

#[derive(Subcommand, Debug)]
enum Commands {
    #[clap(about = "Bootstrap the Twizzler Rust toolchain.")]
    Bootstrap(BootstrapOptions),
    #[clap(about = "Run cargo check on the codebase.")]
    Check(CheckOptions),
    #[clap(about = "Build the Twizzler system.")]
    Build(BuildOptions),
    #[clap(about = "Build a bootable disk image.")]
    Doc(DocOptions),
    #[clap(about = "Build a bootable disk image.")]
    MakeImage(ImageOptions),
    #[clap(about = "Boot a disk image in Qemu.")]
    StartQemu(QemuOptions),
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    if let Some(command) = cli.command {
        match command {
            Commands::Bootstrap(x) => toolchain::do_bootstrap(x),
            Commands::Check(x) => build::do_check(x),
            Commands::Build(x) => build::do_build(x).map(|_| ()),
            Commands::Doc(x) => build::do_docs(x).map(|_| ()),
            Commands::MakeImage(x) => image::do_make_image(x).map(|_| ()),
            Commands::StartQemu(x) => qemu::do_start_qemu(x),
        }
    } else {
        anyhow::bail!("you must specify a subcommand.");
    }
}

fn print_status_line(name: &str, config: Option<&BuildConfig>) {
    if let Some(config) = config {
        eprintln!(
            "=== BUILDING {} [{}-{}::{}]",
            name,
            config.arch.to_string(),
            config.machine.to_string(),
            config.profile.to_string()
        );
    } else {
        eprintln!("=== BUILDING {} [build::release]", name);
    }
}
