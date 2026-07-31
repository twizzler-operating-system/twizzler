//! Scenario-driven system testing.
//!
//! `start-qemu` stays the interactive development path; this drives named scenarios on top of the
//! same [`crate::qemu::run_once`] primitive and decides pass/fail itself.

use clap::{Args, ValueEnum};

use crate::{
    qemu::{self, print_report, RunConfig},
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

/// The lowest `-m` size (in MB) the suite is known to complete under, found by hand-bisecting
/// `cargo test-all --scenario lowmem --memory {4096,2048,1024,512}` on 2026-07-31. 512 hung (the
/// suite needs more than one `memhog-test` round's worth of headroom to make progress); 1024
/// completed cleanly and repeatably.
const LOWMEM_DEFAULT_MB: u32 = 1024;

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
    #[clap(long, help = "Don't build anything, just run against the current image")]
    pub no_build: bool,
    #[clap(
        long,
        help = "Override the scenario's guest memory size in MB (currently only read by --scenario lowmem; used to bisect the memory floor)"
    )]
    pub memory: Option<u32>,
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
            // Leave the gdb serial port unbound; scenarios are run unattended, and binding it
            // would collide between concurrent runs.
            gdb: 0,
            no_build: self.no_build,
            no_test_monitor: false,
        }
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
        label: "default".to_string(),
        monitor: true,
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
        label: "lowmem".to_string(),
        monitor: true,
        heartbeat_tries: LOWMEM_HEARTBEAT_TRIES,
    };
    run_and_report(cli, run)
}

/// Boot `image` with `run` and report what the guest's test suite said. Shared by every scenario
/// that just runs the normal test suite under a different `RunConfig`.
fn run_and_report(cli: &TestOptions, run: RunConfig) -> anyhow::Result<()> {
    let options = cli.qemu_options(true);
    let image = if options.no_build {
        qemu::prebuilt_image_path(&options.config)
    } else {
        crate::image::do_make_image((&options).into())?.disk_image
    };

    let mut outcome = qemu::run_once(&options, &run, &image)?;

    if let Some(log) = &outcome.serial_log {
        println!("serial log: {}", log.display());
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
        eprintln!("FAILED: {} of {} tests failed", report.failed(), report.tests.len());
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
