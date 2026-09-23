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
    /// The standard suite under constrained guest memory with a resident hog pinning most of it,
    /// so the demand-paged test binaries have to be replaced rather than cached.
    Lowmem,
}

/// Default guest memory (in MB) for `--scenario lowmem`.
///
/// The tightest size the suite passes at, measured 2026-09-11 on release/kvm/smp4 (4/4 clean).
/// The ladder below it, same build, is a cliff rather than a slope:
///
/// | MB | outcome |
/// |------|--------------------------------------------------------------------------------|
/// | 1536 | pass, and *zero* memory-pressure prints -- no page-cache pressure at all |
/// | 1408 | pass, zero pressure |
/// | 1280 | pass, zero pressure (this default) |
/// | 1152 | wedge in the *pager*: `SyncRegion`/`ObjectEvict` time out, `kq submission stuck` |
/// | 1024 | wedge in the frame allocator: page data 57%, kernel 40%, idle 1%, `r: 0`, 4 waiters |
///
/// So there is currently no size that both squeezes the page cache and completes: it goes from no
/// pressure straight into one of two defects. That matters because squeezing the page cache is the
/// point of this scenario -- the read-only test binaries are demand-paged from the data disk, and
/// forcing the kernel to *replace* them is what it is meant to exercise. Reaching that needs both
/// of the above fixed, and the 1024 one is a missing feature rather than a bug: `reclaim_main`'s
/// steps 1-5 ("reclaim unused, backed object memory", "cache replacement clean objects") are still
/// a `// TODO`, so nothing in the kernel can drop a clean backed page. 1024 is where to set this
/// once they land.
///
/// A previous note here recorded the bootloader dying at 1024 (`PANIC: High memory allocator: Out
/// of memory`) and a `test_condvar` wedge at 2048. Both are gone: the wedge was the thread reaper
/// starting *after* `test_main()` (see `boot_sequence`), which left 500 of that test's spawns
/// holding a 2 MiB kernel stack each, and with that fixed 1024 boots well past `pager ready`.
const LOWMEM_DEFAULT_MB: u32 = 1280;

/// Free memory (in MB) the resident hog leaves before the suite starts under `--scenario lowmem`.
///
/// Guest size alone never squeezed the page cache: every size that completed ran with zero
/// pressure (the table above), so shrinking the guest tested the memory floor rather than
/// replacement. Pinning everything but this much instead caps what the ~850 MiB of demand-paged
/// test binaries can keep resident, whatever the guest size, and every page past the cap has to be
/// replaced. Init starts the hog (`memhog`, via `--memhog-free`) and waits for it to hold its
/// memory before the suite runs.
const LOWMEM_MEMHOG_FREE_MB: u32 = 256;

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
        help = "Run this program in init instead of the test suite, and report the guest's exit \
                code rather than a test report. The first word names the program (a bare name is \
                resolved under /initrd) and the rest are its arguments, e.g. \
                --autostart=\"pagepar /sysroot/lib 4 16\"."
    )]
    pub autostart: Option<String>,
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
        help = "Before the suite (or autostart program) runs, have init start a resident hog that \
                pins memory until only this many MB are free. --scenario lowmem defaults it on; \
                0 disables. Baked into the image's kernel command line, so it is ignored with \
                --boot-image or --no-build."
    )]
    pub memhog_free: Option<u32>,
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
        help = "Use this ext4 disk (nvme) instead of the shared target/disk-<triple>.img."
    )]
    pub disk_image: Option<PathBuf>,
    #[clap(
        long,
        help = "Run this benchmark crate (optionally followed by libtest name filters) before the \
                test suite, as `start-qemu --bench` does. Quote the whole thing: \
                --bench=\"sysbench page_fault_zero_fill\"."
    )]
    pub bench: Option<String>,
    #[clap(
        long,
        default_value_t = 1,
        help = "Run --bench this many times in one boot."
    )]
    pub bench_iters: usize,
    #[clap(
        long,
        help = "Discard guest writes to the boot and data images, so concurrent runs can share one \
                image instead of each copying it. See the flag of the same name on start-qemu."
    )]
    pub snapshot_disks: bool,
    #[clap(
        long,
        help = "Host port to forward to the guest's ssh port. 0 allocates one dynamically.",
        default_value_t = crate::qemu::DEFAULT_QEMU_PORT
    )]
    pub ssh_port: u16,
    #[clap(
        long,
        help = "After the guest exits, check the ext4 data disk (the nvme one, --disk-image or \
                target/disk-<triple>.img) with the host's e2fsck and report what it found. \
                Read-only: nothing is repaired. Fails the run with exit 38 if the filesystem is \
                not clean."
    )]
    pub fsck: bool,
}

impl TestOptions {
    /// Build the qemu options for a run. Scenarios own the knobs that decide *how* the system
    /// boots, so those are set here rather than exposed on the command line.
    fn qemu_options(&self, tests: bool, memhog_free: u32) -> QemuOptions {
        let mut kernel_arg = self.kernel_arg.clone();
        if memhog_free > 0 {
            kernel_arg.push(format!("--memhog-free={memhog_free}"));
        }
        QemuOptions {
            config: self.config,
            qemu_options: self.qemu_options.clone(),
            tests,
            benches: false,
            bench: self.bench.clone(),
            bench_iters: self.bench_iters,
            sysbench: false,
            kernel: false,
            data: None,
            repeat: false,
            autostart: self.autostart.clone(),
            kernel_arg,
            // Leave the gdb serial port unbound; scenarios are run unattended, and binding it
            // would collide between concurrent runs.
            gdb: 0,
            // An explicit boot image is by definition already built.
            no_build: self.no_build || self.boot_image.is_some(),
            no_test_monitor: false,
            kvm: self.kvm.clone(),
            disk_image: self.disk_image.clone(),
            snapshot_disks: self.snapshot_disks,
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
    run_and_report(cli, run, cli.memhog_free.unwrap_or(0))
}

/// Boot the standard suite under constrained guest memory with the resident hog pinning all but
/// [`LOWMEM_MEMHOG_FREE_MB`] of it, with a longer wait budget since low-memory boots are much
/// slower.
fn run_lowmem(cli: &TestOptions) -> anyhow::Result<()> {
    let mb = cli.memory.unwrap_or(LOWMEM_DEFAULT_MB);
    let run = RunConfig {
        memory: format!("{mb},slots=4,maxmem=128G"),
        label: cli.label("lowmem"),
        monitor: true,
        heartbeat_tries: LOWMEM_HEARTBEAT_TRIES,
        serial_log: cli.serial_log.clone(),
    };
    run_and_report(cli, run, cli.memhog_free.unwrap_or(LOWMEM_MEMHOG_FREE_MB))
}

/// Exit code for "the run itself was fine, but the data disk did not survive it". Distinct from
/// 33 (tests failed), 34 (no report) and 35-37 (guest death) so a sweep can tell them apart.
const FSCK_EXIT: i32 = 38;

/// Check the ext4 data disk the run just used, printing whatever e2fsck found. Returns whether it
/// came back clean; anything that stops the check from happening at all counts as not clean, since
/// the caller asked for the answer and there isn't one.
fn check_disk(cli: &TestOptions) -> bool {
    let image = crate::disk::image_path(&cli.config.twz_triple(), cli.disk_image.as_deref());
    if cli.snapshot_disks {
        println!(
            "note: --snapshot-disks put this run's writes in a temporary overlay, so fsck sees \
             {} as it was before the boot",
            image.display()
        );
    }
    if !image.is_file() {
        eprintln!("FSCK FAILED: {} does not exist", image.display());
        return false;
    }

    // Another invocation may be writing this same image right now (the default one is shared
    // per-triple). Without the lock a check can read a half-written filesystem and call the writer
    // corrupt.
    let _lock = crate::imagelock::image_lock(&image)
        .inspect_err(|e| eprintln!("warning: checking {} unlocked: {}", image.display(), e))
        .ok();

    match crate::disk::fsck(&image) {
        Ok(report) if report.clean() => {
            println!("fsck: {} is clean", image.display());
            true
        }
        Ok(report) => {
            eprintln!("FSCK FAILED: {}: {}", image.display(), report.describe());
            eprint!("{}", report.output);
            false
        }
        Err(e) => {
            eprintln!("FSCK FAILED: could not check {}: {:#}", image.display(), e);
            false
        }
    }
}

/// Boot `image` with `run` and report what the guest's test suite said. Shared by every scenario
/// that just runs the normal test suite under a different `RunConfig`. `memhog_free` is the
/// resident hog's free-memory target in MB (0 for no hog), baked into the image's command line.
fn run_and_report(cli: &TestOptions, run: RunConfig, memhog_free: u32) -> anyhow::Result<()> {
    // An autostart run builds a *non*-test image on purpose. init runs the suite before it reaches
    // the autostart program and shuts the guest down at the end of it, so a test-enabled image
    // would never run the program at all.
    let options = cli.qemu_options(cli.autostart.is_none(), memhog_free);
    if memhog_free > 0 && options.no_build {
        println!(
            "note: --memhog-free={memhog_free} needs an image built by this run; the image being \
             booted keeps whatever command line it was built with"
        );
    }
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

    // Checked before anything below decides to exit: a guest that died or failed its tests is
    // exactly the case where the state it left on disk is worth looking at.
    let dirty_disk = cli.fsck && !check_disk(cli);

    // A dead guest is its own outcome, and a more useful one than "no report": the run did not
    // exhaust its budget, it died, and we stopped it.
    if let Some(death) = outcome.guest_death {
        eprintln!("FAILED: {}", death.describe());
        std::process::exit(death.exit_code());
    }

    // An autostart run has no test suite to report, so its outcome is the exit code init handed to
    // isa-debug-exit. A run that never got there produced nothing and is a failure, same as a
    // missing report below.
    if let Some(autostart) = &cli.autostart {
        return match outcome.guest_code {
            Some(0) if dirty_disk => {
                eprintln!(
                    "FAILED: autostart {} exited 0, but the data disk is not clean",
                    autostart
                );
                std::process::exit(FSCK_EXIT);
            }
            Some(0) => {
                println!("autostart {} exited 0", autostart);
                Ok(())
            }
            Some(code) => {
                eprintln!("FAILED: autostart {} exited with code {}", autostart, code);
                std::process::exit(33);
            }
            None => {
                eprintln!("FAILED: autostart {} never shut the guest down", autostart);
                std::process::exit(34);
            }
        };
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
        Some(_) if dirty_disk => {
            eprintln!(
                "FAILED: all {} tests passed, but the data disk is not clean",
                report.tests.len()
            );
            std::process::exit(FSCK_EXIT);
        }
        Some(_) => {
            println!("all {} tests passed", report.tests.len());
            Ok(())
        }
    }
}
