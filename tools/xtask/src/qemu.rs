use std::{
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    str::FromStr,
    time::Duration,
};

use unittest_report::{ReportInfo, ReportStatus};

use crate::{
    toolchain::get_toolchain_path,
    triple::{Arch, Machine},
    QemuOptions,
};

const DEFAULT_QEMU_PORT: u16 = 5555;

/// Default guest memory, in the form accepted by qemu's `-m`.
pub const DEFAULT_MEMORY: &str = "12000,slots=4,maxmem=128G";

/// Knobs a test scenario can vary for a single qemu run, on top of `QemuOptions`.
///
/// Defaults reproduce exactly what `start-qemu` does today, so a scenario only has to name what it
/// actually wants to change.
#[derive(Debug, Clone)]
pub struct RunConfig {
    /// Value for qemu's `-m`.
    pub memory: String,
    /// Names the run in log files, e.g. `lowmem-3`.
    pub label: String,
    /// Watch the serial console for a test report, and bound the run's wall time.
    pub monitor: bool,
    /// How many heartbeat pokes to send before giving up on the run.
    pub heartbeat_tries: usize,
}

impl Default for RunConfig {
    fn default() -> Self {
        Self {
            memory: DEFAULT_MEMORY.to_string(),
            label: "run".to_string(),
            monitor: false,
            heartbeat_tries: 20,
        }
    }
}

/// What happened during a single qemu run. Deliberately just data: `run_once` never decides whether
/// a run "passed", so scenarios can apply their own criteria.
#[derive(Debug)]
pub struct RunOutcome {
    /// `None` if qemu had to be killed for exceeding its time budget.
    pub qemu_exit: Option<ExitStatus>,
    /// The code the guest passed to `sys_debug_shutdown`, recovered from qemu's exit status.
    /// `None` if the guest never asked to shut down (qemu exited for its own reasons, or we
    /// killed it).
    pub guest_code: Option<i32>,
    /// The parsed test report, if the guest produced one.
    pub report: Option<ReportInfo>,
    /// Serial console transcript for this run, if the run captured one.
    pub serial_log: Option<PathBuf>,
    /// Host port forwarded to the guest's ssh port.
    pub ssh_port: u16,
    /// Why a scenario's post-boot hook failed, if it ran and failed.
    pub hook_error: Option<String>,
}

impl RunOutcome {
    /// True if qemu exited the way a passing test run exits.
    ///
    /// The guest signals shutdown through the isa-debug-exit device, which makes qemu exit with
    /// `(code << 1) | 1`; the guest passes 0, so a healthy test run yields 1 rather than 0.
    pub fn qemu_ok(&self) -> bool {
        match self.qemu_exit {
            Some(es) => es.success() || es.code() == Some(1),
            None => false,
        }
    }
}

/// Recover the code a guest passed to `sys_debug_shutdown` from qemu's exit status.
///
/// The isa-debug-exit device turns a guest write of `code` into a qemu exit status of
/// `(code << 1) | 1`, so only odd statuses carry a guest code; anything else means qemu exited on
/// its own terms.
fn decode_guest_code(status: ExitStatus) -> Option<i32> {
    let code = status.code()?;
    (code & 1 == 1).then(|| (code - 1) / 2)
}

#[derive(Debug)]
struct QemuCommand {
    cmd: Command,
    arch: Arch,
    machine: Machine,
}

impl QemuCommand {
    pub fn new(cli: &QemuOptions) -> Self {
        let cmd = match cli.config.arch {
            Arch::X86_64 => String::from("qemu-system-x86_64"),
            Arch::Aarch64 => {
                if cli.config.machine == Machine::Morello {
                    // all morello software by default is installed in ~/cheri
                    let mut qemu = home::home_dir().expect("failed to find home directory");
                    qemu.push("cheri/output/sdk/bin/qemu-system-morello");
                    String::from(qemu.to_str().unwrap())
                } else {
                    String::from("qemu-system-aarch64")
                }
            }
        };
        Self {
            cmd: Command::new(&cmd),
            arch: cli.config.arch,
            machine: cli.config.machine,
        }
    }

    /// Build the qemu command line. Returns the host port forwarded to the guest's ssh port, which
    /// the network scenario needs in order to drive an ssh client at the guest.
    pub fn config(&mut self, options: &QemuOptions, disk_image: PathBuf, run: &RunConfig) -> u16 {
        // Set up the basic stuff, memory and bios, etc.
        self.cmd.arg("-m").arg(&run.memory);

        // configure architechture specific parameters
        self.arch_config();

        // Connect disk image
        self.cmd.arg("-drive").arg(format!(
            "format=raw,file={}",
            disk_image.as_path().display()
        ));

        let disk_image_path = format!(
            "target/disk-{}.img",
            options.config.twz_triple().to_string()
        );
        if !std::fs::exists(&disk_image_path).unwrap() {
            crate::disk::create_fresh_disk_image(&options.config.twz_triple()).unwrap();
        }

        let nvme_drive = format!("file={},if=none,id=nvme", disk_image_path);
        self.cmd
            .arg("-drive")
            .arg(nvme_drive)
            .arg("-device")
            .arg("nvme,serial=deadbeef,drive=nvme");

        self.cmd
            .arg("-device")
            .arg("virtio-pmem-pci,memdev=dataset,id=nv2");
        let mem_drive = format!(
            "memory-backend-file,id=dataset,size=107374182400,mem-path={},share=on",
            disk_image_path
        );
        self.cmd.arg("-object").arg(mem_drive);

        self.cmd.arg("-device").arg("virtio-net-pci,netdev=net0");

        let port = {
            let listener = match TcpListener::bind(format!("0.0.0.0:{}", DEFAULT_QEMU_PORT)) {
                Ok(l) => l,
                Err(_) => {
                    println!(
                        "Failed to allocate default port {} on host, dynamically assigning.",
                        DEFAULT_QEMU_PORT
                    );
                    match TcpListener::bind("0.0.0.0:0") {
                        Ok(l) => l,
                        Err(e) => {
                            panic!("Port allocation for Qemu failed! {}", e);
                        }
                    }
                }
            };

            listener
                .local_addr()
                .expect("Expected to get local address.")
                .port()
        };

        println!("Allocated port {} for Qemu!", port);

        self.cmd.arg("-netdev").arg(format!(
            "user,id=net0,hostfwd=tcp::{}-:{}",
            port, DEFAULT_QEMU_PORT
        ));

        self.cmd
            .arg("--no-reboot") // exit instead of rebooting
            .arg("-serial")
            .arg("mon:stdio");
        //-serial mon:stdio creates a multiplexed stdio backend connected
        // to the serial port and the QEMU monitor, and
        // -nographic also multiplexes the console and the monitor to stdio.

        if options.gdb != 0 {
            let gdb_port = {
                let listener = match TcpListener::bind(format!("0.0.0.0:{}", options.gdb)) {
                    Ok(l) => l,
                    Err(_) => {
                        println!(
                            "Failed to allocate default gdb port {} on host, dynamically assigning.",
                            options.gdb
                        );
                        match TcpListener::bind("0.0.0.0:0") {
                            Ok(l) => l,
                            Err(e) => {
                                panic!("gdb port alloc failed! {}", e);
                            }
                        }
                    }
                };
                listener.local_addr().expect("local gdb addr").port()
            };

            println!("gdb debugging port: {}", gdb_port);
            self.cmd
                .arg("-serial")
                .arg(&format!("tcp::{},server,nowait", gdb_port));
        }

        self.cmd.arg("-vga").arg("virtio");

        // add additional options for qemu
        self.cmd.args(&options.qemu_options);

        println!("qemu: {:?}", self.cmd);

        //self.cmd.arg("-smp").arg("4,sockets=1,cores=2,threads=2");

        port
    }

    fn arch_config(&mut self) {
        let mut ovmf = get_toolchain_path().unwrap();
        match self.arch {
            Arch::X86_64 => {
                // bios, platform
                ovmf.push("OVMF.fd");
                self.cmd.arg("-bios").arg(ovmf);
                self.cmd.arg("-machine").arg("q35,nvdimm=on");

                // Attach the exit device unconditionally: without it, a guest calling
                // sys_debug_shutdown writes to a nonexistent port and the run just hangs until it
                // times out. Test runs are not the only ones that want to end themselves.
                self.cmd
                    .arg("-device")
                    .arg("isa-debug-exit,iobase=0xf4,iosize=0x04");

                let has_kvm = std::env::consts::ARCH == self.arch.to_string()
                    && Path::new("/dev/kvm").exists();

                if has_kvm {
                    self.cmd.arg("-enable-kvm");
                    self.cmd
                        .arg("-cpu")
                        .arg("host,+x2apic,+tsc-deadline,+invtsc,+tsc,+rdtscp");
                } else {
                    self.cmd.arg("-cpu").arg("max");
                }

                // Connect some nvdimms
                /*
                self.cmd.arg("-object").arg(format!(
                    "memory-backend-file,id=mem1,share=on,mem-path={},size=4G",
                    make_path(build_info, true, "pmem.img")
                ));
                self.cmd.arg("-device").arg("nvdimm,id=nvdimm1,memdev=mem1");
                */
            }
            Arch::Aarch64 => {
                ovmf.push("OVMF-AA64.fd");
                self.cmd.arg("-bios").arg(ovmf);
                self.cmd.arg("-net").arg("none");
                if self.machine == Machine::Morello {
                    self.cmd.arg("-machine").arg("virt,gic-version=3");
                    self.cmd.arg("-cpu").arg("morello");
                } else {
                    // use qemu virt machine by default
                    // virt uses GICv2 by default
                    self.cmd.arg("-machine").arg("virt");
                    self.cmd.arg("-cpu").arg("cortex-a72");
                }
                self.cmd.arg("-nographic");
            }
        }
    }
}

/// Print a per-test table plus a summary, and list the failures again at the end so they are
/// visible without scrolling back through the boot log.
pub(crate) fn print_report(report: &ReportInfo) {
    let name_width = report
        .tests
        .iter()
        .map(|t| t.name.len())
        .max()
        .unwrap_or(0)
        .max(4);

    println!();
    println!("{:<width$}  {:>8}  status", "test", "time", width = name_width);
    for test in &report.tests {
        println!(
            "{:<width$}  {:>7}s  {}",
            test.name,
            format!("{:.2}", test.duration.as_secs_f64()),
            test.status,
            width = name_width,
        );
    }

    let failed = report.failed();
    let total = report.tests.len();
    println!();
    println!(
        "TEST RESULTS: {} passed, {} failed, {} total -- time: {:2} seconds",
        total - failed,
        failed,
        total,
        report.time.as_millis() as f64 / 1000.0,
    );

    if failed > 0 {
        println!("failures:");
        for test in report.tests.iter().filter(|t| !t.status.passed()) {
            println!("    {}: {}", test.name, test.status);
        }
    }
}

/// Where `--no-build` expects to find an already-built boot image.
pub(crate) fn prebuilt_image_path(config: &crate::BuildConfig) -> PathBuf {
    PathBuf::from(format!(
        "target/kernel/{}-unknown-none/{}/disk.img",
        config.arch.to_string(),
        config.profile.to_string()
    ))
}

fn serial_log_path(label: &str) -> PathBuf {
    PathBuf::from("target/test-logs").join(format!("{}.log", label))
}

/// Stream qemu's serial output to our stdout and, if we have one, to a transcript file, returning
/// the first complete test report seen.
fn read_serial(
    stdout: Option<std::process::ChildStdout>,
    mut log: Option<std::fs::File>,
) -> Option<ReportInfo> {
    let stdout = stdout?;
    let mut found = None;
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else {
            continue;
        };
        let line = line.trim();
        println!(" ==> {}", line);
        if let Some(log) = log.as_mut() {
            let _ = writeln!(log, "{}", line);
        }
        // Keep draining once a report has landed rather than dropping the pipe: anything the
        // guest prints afterwards would otherwise fill it and wedge qemu.
        if found.is_none() {
            if let Some(rest) = line.strip_prefix("REPORT ") {
                if let Ok(ReportStatus::Ready(report)) =
                    unittest_report::Report::from_str(rest.trim()).map(|report| report.status)
                {
                    found = Some(report);
                }
            }
        }
    }
    found
}

/// Boot `image` in qemu once and report what happened.
///
/// This deliberately never calls `std::process::exit`: it reports outcomes and lets the caller
/// decide what counts as success, so that scenarios which run several boots can keep going.
pub(crate) fn run_once(
    options: &QemuOptions,
    run: &RunConfig,
    image: &Path,
) -> anyhow::Result<RunOutcome> {
    use wait_timeout::ChildExt;

    let mut run_cmd = QemuCommand::new(options);
    let ssh_port = run_cmd.config(options, image.to_path_buf(), run);

    // Only capture qemu's stdio when we plan to talk to the guest; an interactive boot needs the
    // terminal wired straight through.
    if run.monitor {
        run_cmd.cmd.stdin(Stdio::piped());
        run_cmd.cmd.stdout(Stdio::piped());
    }

    let mut child = run_cmd.cmd.spawn()?;
    let mut child_stdin = child.stdin.take();
    let child_stdout = child.stdout.take();

    let serial_log = run.monitor.then(|| serial_log_path(&run.label));
    let log_file = serial_log.as_ref().and_then(|path| {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        std::fs::File::create(path)
            .inspect_err(|e| eprintln!("failed to open serial log {}: {}", path.display(), e))
            .ok()
    });
    let logging_to = serial_log.as_ref().filter(|_| log_file.is_some()).cloned();

    let reader_thread = std::thread::spawn(move || read_serial(child_stdout, log_file));

    let exit_status = if run.monitor {
        let mut tries = 0;
        loop {
            if let Some(es) = child.wait_timeout(Duration::from_secs(15))? {
                break Some(es);
            }
            let Some(stdin) = child_stdin.as_mut() else {
                break None;
            };
            if stdin.write_all(b"status\n").is_err() {
                // qemu closed its end, so it is already on its way out; collect its status
                // instead of calling this a timeout.
                break child.wait_timeout(Duration::from_secs(15))?;
            }
            tries += 1;
            if tries > run.heartbeat_tries {
                break None;
            }
        }
    } else {
        Some(child.wait()?)
    };

    if exit_status.is_none() {
        // Nothing else will reap it, and the reader thread stays blocked on its pipe until it dies.
        let _ = child.kill();
    }

    let report = reader_thread.join().ok().flatten();

    Ok(RunOutcome {
        qemu_exit: exit_status,
        guest_code: exit_status.and_then(decode_guest_code),
        report,
        serial_log: logging_to,
        ssh_port,
        hook_error: None,
    })
}

pub(crate) fn do_start_qemu(cli: QemuOptions) -> anyhow::Result<()> {
    let monitor = cli.tests && !cli.no_test_monitor;
    let image = if cli.no_build {
        prebuilt_image_path(&cli.config)
    } else {
        crate::image::do_make_image((&cli).into())?.disk_image
    };

    let run = RunConfig {
        label: "start-qemu".to_string(),
        monitor,
        ..Default::default()
    };
    let outcome = run_once(&cli, &run, &image)?;

    let Some(exit_status) = outcome.qemu_exit else {
        eprintln!("qemu timed out");
        std::process::exit(34);
    };

    if let Some(report) = outcome.report {
        print_report(&report);
    } else if monitor {
        eprintln!("qemu didn't produce report");
        std::process::exit(34);
    }

    if exit_status.success() {
        if cli.repeat {
            return do_start_qemu(cli);
        }
        Ok(())
    } else {
        if cli.tests || cli.benches || cli.bench.is_some() {
            if exit_status.code().unwrap() == 1 {
                eprintln!("qemu reports tests passed");
                if cli.repeat {
                    return do_start_qemu(cli);
                }
                std::process::exit(0);
            } else {
                eprintln!("qemu reports tests failed");
                std::process::exit(33);
            }
        }
        anyhow::bail!("qemu return with error");
    }
}
