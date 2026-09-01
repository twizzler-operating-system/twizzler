#![feature(iterator_try_collect)]

mod build;
mod disk;
mod image;
mod imagelock;
mod qemu;
mod test;
mod toolchain;
mod triple;

use std::{fmt::Display, path::PathBuf};

use clap::{Args, Parser, Subcommand, ValueEnum};
use qemu::{KvmOptions, DEFAULT_QEMU_PORT};
use toolchain::ToolchainCommands;
use tracing::Level;
use triple::{Arch, Machine, Triple};

#[derive(Parser, Debug)]
#[clap(name = "xtask", author = "Daniel Bittman <danielbittman1@gmail.com>", version = "1.0", about = "Build system for Twizzler", long_about = None)]
struct Cli {
    /// Use a specific installed toolchain instead of the one implied by the current submodule
    /// pointers. Accepts a tag (`toolchain_a-b-c`), a bare hash triple, or a path. Also settable
    /// via TWIZZLER_TOOLCHAIN.
    #[clap(long = "toolchain", global = true, value_name = "TAG|PATH")]
    toolchain_dir: Option<String>,
    #[clap(subcommand)]
    command: Option<Commands>,
}

#[derive(Debug, ValueEnum, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Profile {
    /// Cargo's `dev`, where the root manifest optimizes dependencies but leaves workspace crates
    /// unoptimized. This is the one the debug configurations in `many.py` build.
    Debug,
    Release,
    /// `dev` with every one of those overrides reset to `opt-level = 0`, for when the thing being
    /// investigated is codegen itself and dependencies need to be unoptimized too.
    FullDebug,
}

impl Profile {
    /// The cargo profile to request, or `None` to take cargo's default (`dev`).
    ///
    /// Also the build directory name, via `Display` -- which is why `Debug` maps to `debug`: that
    /// is where cargo puts `dev` output.
    fn requested(&self) -> Option<&'static str> {
        match self {
            Profile::Debug => None,
            Profile::Release => Some("release"),
            Profile::FullDebug => Some("full-debug"),
        }
    }

    /// The profile to request for collections compiled out of a crate's own manifest rather than
    /// the root workspace's -- see `build::build_third_party`.
    ///
    /// Those manifests define only cargo's built-in profiles, so asking for `full-debug` there
    /// fails the whole build with "profile `full-debug` is not defined". Falling back to `dev` is
    /// what the profile means anyway: it exists to unoptimize *workspace* code, and a port is a
    /// dependency however it is built.
    fn requested_foreign_manifest(&self) -> Option<&'static str> {
        match self {
            Profile::FullDebug => Profile::Debug.requested(),
            other => other.requested(),
        }
    }
}

impl Display for Profile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let str = match self {
            Profile::Debug => "debug",
            Profile::Release => "release",
            Profile::FullDebug => "full-debug",
        };
        write!(f, "{str}")
    }
}

impl Default for Profile {
    fn default() -> Self {
        Self::Debug
    }
}

#[derive(Args, Debug, Clone, Copy)]
struct BuildConfig {
    #[clap(short, long, value_enum, default_value_t = Profile::Debug, help = "Select build profile.")]
    pub profile: Profile,
    #[clap(short, long, value_enum, default_value_t = Arch::X86_64, help = "Select target architecture.")]
    pub arch: Arch,
    #[clap(short, long, value_enum, default_value_t = Machine::Unknown, help = "Select target machine.")]
    pub machine: Machine,
}

impl BuildConfig {
    fn is_default_arch(&self) -> bool {
        self.arch == Arch::X86_64
    }

    pub fn is_default_machine(&self) -> bool {
        self.machine == Machine::Unknown
    }

    pub fn is_default_target(&self) -> bool {
        self.is_default_arch() && self.is_default_machine()
    }

    pub fn twz_triple(&self) -> Triple {
        // Compiling for aarch64 requires specifying the machine it will be compiled
        // for. However, the supported triples have a generic machine value of unknown.
        // We set the default machine value to unknown in this case.
        let machine = if self.arch == Arch::Aarch64 {
            Machine::Unknown
        } else {
            self.machine
        };
        Triple::new(self.arch, machine, triple::Host::Twizzler, None)
    }
}

#[derive(Args, Debug)]
struct BuildOptions {
    #[clap(flatten)]
    pub config: BuildConfig,
    #[clap(long, short, help = "Build tests-enabled system.")]
    tests: bool,
    #[clap(
        long,
        help = "Also build the test-only programs (crates marked twizzler-build = \"test\"). \
                Implied by --tests; on its own it builds them without the #[test] collection."
    )]
    test_programs: bool,
    #[clap(long, short, help = "Only build kernel part of system.")]
    kernel: bool,
    #[clap(long, short, help = "Only build runtime part of system.")]
    only_runtime: bool,
}

#[derive(Args, Debug)]
struct DocOptions {
    #[clap(flatten)]
    pub config: BuildConfig,
}

#[derive(ValueEnum, Debug, Clone, Copy)]
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
    #[clap(short, long)]
    package: Option<String>,
    #[clap(flatten)]
    pub config: BuildConfig,
    #[clap(long, short)]
    pub manifest_path: Option<PathBuf>,
    #[clap(long, short, value_enum, default_value_t = MessageFormat::Human)]
    pub message_format: MessageFormat,
    #[clap(long, short)]
    pub workspace: bool,
    #[clap(long, short, help = "Only build kernel part of system.")]
    kernel: bool,
    #[clap(long)]
    all_targets: bool,
    #[clap(long)]
    keep_going: bool,
    #[clap(long)]
    bin: Option<String>,
}

#[derive(Args, Debug, Clone)]
struct ImageOptions {
    #[clap(flatten)]
    pub config: BuildConfig,
    #[clap(long, short, help = "Build tests-enabled system.")]
    tests: bool,
    #[clap(long, help = "Build benchmark-enabled system.")]
    benches: bool,
    #[clap(long, short, help = "Select a single program to bench.")]
    bench: Option<String>,
    #[clap(
        long,
        default_value_t = 1,
        help = "Run --bench this many times in one boot. Later passes see the kernel state the \
                earlier ones left, which is what surfaces drift within a boot."
    )]
    bench_iters: usize,
    #[clap(long, short, help = "Only build kernel part of system.")]
    kernel: bool,
    #[clap(long, short, help = "Share a file/directory with Twizzler")]
    data: Option<PathBuf>,
    #[clap(long, help = "Auto-start a program in init.")]
    autostart: Option<String>,
    #[clap(
        long,
        allow_hyphen_values = true,
        help = "Append an argument to the kernel command line, which is baked into the image. \
                May be specified multiple times. Arguments starting with a dash need the equals \
                form, e.g. --kernel-arg=--no-pcid."
    )]
    kernel_arg: Vec<String>,
    #[clap(
        long,
        help = "Create or update the ext4 data disk at this path instead of the shared \
                target/disk-<triple>.img. Concurrent builds that each pass their own path never \
                touch the build tree's copy, and so never queue behind (or corrupt) each other."
    )]
    pub disk_image: Option<PathBuf>,
}

#[derive(Subcommand, ValueEnum, Debug, Clone, Copy)]
enum DiskCmd {
    Reset,
    Setup,
}

#[derive(Args, Debug, Clone)]
struct DiskImageOptions {
    #[clap(flatten)]
    pub config: BuildConfig,

    #[clap(subcommand)]
    pub cmd: DiskCmd,
    #[clap(
        long,
        short,
        help = "Force copying sysroot to disk image, even if it appears up to date."
    )]
    pub force: bool,
    #[clap(
        long,
        help = "Create or update the ext4 data disk at this path instead of the shared \
                target/disk-<triple>.img. Concurrent builds that each pass their own path never \
                touch the build tree's copy, and so never queue behind (or corrupt) each other."
    )]
    pub disk_image: Option<PathBuf>,
}

impl From<ImageOptions> for BuildOptions {
    fn from(io: ImageOptions) -> Self {
        Self {
            config: io.config,
            tests: io.tests || io.benches || io.bench.is_some(),
            // --autostart is how the harness drives a single program, and the program it names is
            // routinely one of the test-only crates (`--autostart="pagepar ..."`). Those have to
            // be built for it -- but not the whole #[test] collection, which the guest never runs
            // on an autostart boot.
            test_programs: io.tests || io.benches || io.bench.is_some() || io.autostart.is_some(),
            kernel: io.kernel,
            only_runtime: false,
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
    #[clap(
        long,
        help = "Run benchmarks instead of booting normally. Can be used with --tests."
    )]
    benches: bool,
    #[clap(long, short, help = "Select a single program to bench.")]
    bench: Option<String>,
    #[clap(
        long,
        default_value_t = 1,
        help = "Run --bench this many times in one boot. Later passes see the kernel state the \
                earlier ones left, which is what surfaces drift within a boot."
    )]
    bench_iters: usize,
    #[clap(long, short, help = "Only build kernel part of system.")]
    kernel: bool,
    #[clap(long, short, help = "Share a file/directory with Twizzler")]
    data: Option<PathBuf>,
    #[clap(
        long,
        conflicts_with = "benches",
        conflicts_with = "bench",
        help = "Run only the sysbench core-path microbenchmarks (shorthand for --bench sysbench)."
    )]
    sysbench: bool,
    #[clap(
        long,
        short,
        help = "Restart qemu if it exits, unless it returns failure"
    )]
    repeat: bool,
    #[clap(long, help = "Auto-start a program in init.")]
    autostart: Option<String>,
    #[clap(
        long,
        allow_hyphen_values = true,
        help = "Append an argument to the kernel command line, which is baked into the image. \
                May be specified multiple times. Arguments starting with a dash need the equals \
                form, e.g. --kernel-arg=--no-pcid."
    )]
    kernel_arg: Vec<String>,
    #[clap(
        long,
        short,
        help = "Enable GDB connection via serial, exposed via host TCP <port>. Defaults to :2159.",
        default_value_t = 2159
    )]
    gdb: u16,
    #[clap(long, help = "Don't build anything, just start QEMU immediately")]
    no_build: bool,
    #[clap(long, help = "Don't monitor testing system")]
    no_test_monitor: bool,
    #[clap(flatten)]
    kvm: KvmOptions,
    #[clap(
        long,
        help = "Use this ext4 disk (nvme) instead of the shared target/disk-<triple>.img. \
                Point concurrent runs at private copies; qemu takes a write lock on it, unless \
                --snapshot-disks is also given."
    )]
    disk_image: Option<PathBuf>,
    #[clap(
        long,
        help = "Discard guest writes to the boot and data images: qemu opens both read-only and \
                puts writes in a temporary overlay it deletes on exit. Concurrent runs can then \
                share one image instead of each copying it. Anything the guest wrote is gone when \
                it exits."
    )]
    snapshot_disks: bool,
    #[clap(
        long,
        help = "Host port to forward to the guest's ssh port. 0 allocates one dynamically. \
                Concurrent runs must not share a port.",
        default_value_t = DEFAULT_QEMU_PORT
    )]
    ssh_port: u16,
}

impl From<&QemuOptions> for ImageOptions {
    fn from(qo: &QemuOptions) -> Self {
        Self {
            config: qo.config,
            tests: qo.tests,
            benches: qo.benches,
            kernel: qo.kernel,
            data: qo.data.clone(),
            autostart: qo.autostart.clone(),
            kernel_arg: qo.kernel_arg.clone(),
            bench: qo.bench.clone(),
            bench_iters: qo.bench_iters,
            // Build into the same image we are about to boot. Otherwise `--disk-image` boots a
            // private copy while the build writes its binaries into the shared one, which is both
            // wrong and the collision the flag exists to avoid.
            disk_image: qo.disk_image.clone(),
        }
    }
}

#[derive(Subcommand, Debug)]
enum Commands {
    #[clap(subcommand, about = "Manage the Twizzler toolchain(s)")]
    Toolchain(ToolchainCommands),
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
    #[clap(about = "Run a system test scenario.")]
    Test(test::TestOptions),
    #[clap(about = "Create or reset a disk image for a given target.")]
    Disk(DiskImageOptions),
}

fn main() -> anyhow::Result<()> {
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_max_level(Level::WARN)
            .finish(),
    )
    .unwrap();
    let cli = Cli::parse();
    toolchain::set_toolchain_override(cli.toolchain_dir)?;
    if let Some(command) = cli.command {
        match command {
            Commands::Toolchain(x) => toolchain::handle_cli(x),
            Commands::Check(x) => build::do_check(x),
            Commands::Build(x) => build::do_build(x).map(|_| ()),
            Commands::Doc(x) => build::do_docs(x).map(|_| ()),
            Commands::MakeImage(x) => image::do_make_image(x).map(|_| ()),
            Commands::StartQemu(x) => qemu::do_start_qemu(x),
            Commands::Test(x) => test::do_test(x),
            Commands::Disk(x) => disk::do_disk_image(x),
        }
    } else {
        anyhow::bail!("you must specify a subcommand.");
    }
}

fn print_status_line(name: &str, config: Option<&BuildConfig>) {
    if let Some(config) = config {
        eprintln!(
            "=== BUILDING {} [{}-{}::{}]",
            name, config.arch, config.machine, config.profile
        );
    } else {
        eprintln!("=== BUILDING {} [build::release]", name);
    }
}
