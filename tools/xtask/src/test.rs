//! Scenario-driven system testing.
//!
//! `start-qemu` stays the interactive development path; this drives named scenarios on top of the
//! same [`crate::qemu::run_once`] primitive and decides pass/fail itself.

use std::path::PathBuf;

use clap::{Args, ValueEnum};

use crate::{
    qemu::{self, print_report, KvmOptions, RunConfig},
    BuildConfig, QemuOptions,
};

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scenario {
    /// The standard kernel + userspace test suite, as `start-qemu --tests` runs it.
    Default,
    /// The standard suite under constrained guest memory, to exercise the reclaim/pager paths
    /// `memhog-test` is written to stress.
    Lowmem,
}

/// Default guest memory (in MB) for `--scenario lowmem`.
///
/// Measured, not guessed, but the floor is still not established: at 1024 the *bootloader* dies
/// before the kernel ever runs (`PANIC: High memory allocator: Out of memory`, loading the initrd),
/// so no run at that size tested anything. 2048 boots and reaches the kernel test suite, then
/// wedges in the frame allocator during `test_condvar` -- 5 free frames of 287 395, kernel holding
/// 99% of them, one waiter, no forward progress and no panic (exit 36). That wedge is a real defect
/// of its own, not a memory-size choice; see stabilitybugs.md. Bisecting the floor properly needs
/// it fixed first.
const LOWMEM_DEFAULT_MB: u32 = 2048;

/// Low-memory boots are much slower (heavier reclaim/pager traffic); give the suite roughly 3x the
/// default run's wait budget before calling it a hang.
const LOWMEM_HEARTBEAT_TRIES: usize = 60;

#[derive(Args, Debug)]
pub struct TestOptions {
    #[clap(flatten)]
    pub config: BuildConfig,
    #[clap(
        long,
        short,
        value_enum,
        default_value_t = Scenario::Default,
        help = "Which test scenario to run."
    )]
    pub scenario: Scenario,
    #[clap(
        long,
        short,
        help = "Additional options to pass to Qemu. May be specified multiple times."
    )]
    pub qemu_options: Vec<String>,
    #[clap(
        long,
        allow_hyphen_values = true,
        help = "Append an argument to the kernel command line. May be specified multiple times. \
                Arguments starting with a dash need the equals form, e.g. --kernel-arg=--no-pcid. \
                Only takes effect when this run builds its own image: with --boot-image the \
                command line is already baked into the image you pass."
    )]
    pub kernel_arg: Vec<String>,
    #[clap(
        long,
        help = "Don't build anything, just run against the current image"
    )]
    pub no_build: bool,
    #[clap(flatten)]
    pub kvm: KvmOptions,
    #[clap(
        long,
        help = "Override the scenario's guest memory size in MB (currently only read by --scenario lowmem; used to bisect the memory floor)"
    )]
    pub memory: Option<u32>,
    #[clap(
        long,
        help = "Boot this image instead of the one in the build tree. Implies --no-build. Pair with \
                --disk-image to run entirely off private copies, leaving the build tree free."
    )]
    pub boot_image: Option<PathBuf>,
    #[clap(
        long,
        help = "Name this run in the serial log (target/test-logs/<label>.log). Defaults to the \
                scenario name, which collides between concurrent runs."
    )]
    pub label: Option<String>,
    #[clap(
        long,
        help = "Write the serial transcript here instead of into the shared target/test-logs."
    )]
    pub serial_log: Option<PathBuf>,
    #[clap(
        long,
        help = "Use this ext4 disk (nvme + virtio-pmem) instead of the shared target/disk-<triple>.img."
    )]
    pub disk_image: Option<PathBuf>,
    #[clap(
        long,
        help = "Host port to forward to the guest's ssh port. 0 allocates one dynamically.",
        default_value_t = crate::qemu::DEFAULT_QEMU_PORT
    )]
    pub ssh_port: u16,
}

impl TestOptions {
    /// Build the qemu options for a run. Scenarios own the knobs that decide *how* the system
    /// boots, so those are set here rather than exposed on the command line.
    fn qemu_options(&self, tests: bool) -> QemuOptions {
        QemuOptions {
            config: self.config,
            qemu_options: self.qemu_options.clone(),
            tests,
            benches: false,
            bench: None,
            kernel: false,
            data: None,
            repeat: false,
            autostart: None,
            kernel_arg: self.kernel_arg.clone(),
            // Leave the gdb serial port unbound; scenarios are run unattended, and binding it
            // would collide between concurrent runs.
            gdb: 0,
            // An explicit boot image is by definition already built.
            no_build: self.no_build || self.boot_image.is_some(),
            no_test_monitor: false,
            kvm: self.kvm.clone(),
            disk_image: self.disk_image.clone(),
            ssh_port: self.ssh_port,
        }
    }

    /// Names the run's serial log. Scenarios default to their own name, which is fine for one run
    /// at a time and collides the moment two run concurrently.
    fn label(&self, scenario: &str) -> String {
        self.label.clone().unwrap_or_else(|| scenario.to_string())
    }
}

pub(crate) fn do_test(cli: TestOptions) -> anyhow::Result<()> {
    match cli.scenario {
        Scenario::Default => run_default(&cli),
        Scenario::Lowmem => run_lowmem(&cli),
    }
}

/// Boot the test-enabled image and report what the guest's test suite said.
fn run_default(cli: &TestOptions) -> anyhow::Result<()> {
    let run = RunConfig {
        label: cli.label("default"),
        monitor: true,
        serial_log: cli.serial_log.clone(),
        ..Default::default()
    };
    run_and_report(cli, run)
}

/// Boot the standard suite under constrained guest memory (`memhog-test` is what actually leans on
/// this), with a longer wait budget since low-memory boots are much slower.
fn run_lowmem(cli: &TestOptions) -> anyhow::Result<()> {
    let mb = cli.memory.unwrap_or(LOWMEM_DEFAULT_MB);
    let run = RunConfig {
        memory: format!("{mb},slots=4,maxmem=128G"),
        label: cli.label("lowmem"),
        monitor: true,
        heartbeat_tries: LOWMEM_HEARTBEAT_TRIES,
        serial_log: cli.serial_log.clone(),
    };
    run_and_report(cli, run)
}

/// Boot `image` with `run` and report what the guest's test suite said. Shared by every scenario
/// that just runs the normal test suite under a different `RunConfig`.
fn run_and_report(cli: &TestOptions, run: RunConfig) -> anyhow::Result<()> {
    let options = cli.qemu_options(true);
    let image = match &cli.boot_image {
        Some(path) => {
            if !path.is_file() {
                anyhow::bail!("--boot-image {} does not exist", path.display());
            }
            path.clone()
        }
        None if options.no_build => qemu::prebuilt_image_path(&options.config),
        None => crate::image::do_make_image((&options).into())?.disk_image,
    };

    let mut outcome = qemu::run_once(&options, &run, &image)?;

    if let Some(log) = &outcome.serial_log {
        println!("serial log: {}", log.display());
    }

    // A dead guest is its own outcome, and a more useful one than "no report": the run did not
    // exhaust its budget, it died, and we stopped it.
    if let Some(death) = outcome.guest_death {
        eprintln!("FAILED: {}", death.describe());
        std::process::exit(death.exit_code());
    }

    // A run that produced no report tells us nothing, whether it timed out or died early. Treat
    // both as failure rather than falling back on the exit code alone.
    let Some(report) = outcome.report.take() else {
        if outcome.qemu_exit.is_none() {
            eprintln!("FAILED: qemu timed out before producing a report");
        } else {
            eprintln!("FAILED: qemu exited without producing a report");
        }
        std::process::exit(34);
    };

    print_report(&report);

    if !report.all_passed() {
        eprintln!(
            "FAILED: {} of {} tests failed",
            report.failed(),
            report.tests.len()
        );
        std::process::exit(33);
    }

    // The report is authoritative for *what* failed, but a guest that dies after reporting still
    // has to be caught, so cross-check the exit code the guest handed back.
    match outcome.qemu_exit {
        None => {
            eprintln!("FAILED: tests reported passing, but qemu timed out afterwards");
            std::process::exit(34);
        }
        Some(_) if !outcome.qemu_ok() => {
            eprintln!(
                "FAILED: tests reported passing, but the guest exited with code {:?}",
                outcome.guest_code
            );
            std::process::exit(33);
        }
        Some(_) => {
            println!("all {} tests passed", report.tests.len());
            Ok(())
        }
    }
}
