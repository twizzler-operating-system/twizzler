use std::{
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    str::FromStr,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use clap::Args;
use unittest_report::{ReportInfo, ReportStatus};

use crate::{
    toolchain::get_toolchain_path,
    triple::{Arch, Machine},
    QemuOptions,
};

pub const DEFAULT_QEMU_PORT: u16 = 5555;

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
    /// Where to write the transcript. Defaults to `target/test-logs/<label>.log`; set it when the
    /// caller wants the log to land somewhere of its own rather than in the shared directory.
    pub serial_log: Option<PathBuf>,
}

impl Default for RunConfig {
    fn default() -> Self {
        Self {
            memory: DEFAULT_MEMORY.to_string(),
            label: "run".to_string(),
            monitor: false,
            // Overridable because the cap is wall-clock, not progress-based: emulated (non-KVM)
            // runs are far slower than the default allows for.
            heartbeat_tries: std::env::var("TWZ_HEARTBEAT_TRIES")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(20),
            serial_log: None,
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
    /// Set when we concluded the guest was dead and stopped it ourselves, rather than letting the
    /// run burn its whole wall-clock budget producing nothing.
    pub guest_death: Option<GuestDeath>,
}

/// How we decided a guest had died.
#[derive(Debug, Clone, Copy)]
pub enum GuestDeath {
    /// A panic marker was seen on the console.
    Panicked,
    /// The console went silent for this long. Covers panics whose text was garbled by concurrent
    /// cpus as well as guests that wedge without panicking at all.
    Silent(Duration),
    /// The console kept talking but stopped saying anything new for this long, while flooding.
    /// The silence watchdog cannot see this one, and it is how a livelocked guest burns a full
    /// heartbeat budget.
    Livelocked(Duration),
}

impl GuestDeath {
    pub fn describe(&self) -> String {
        match self {
            GuestDeath::Panicked => "guest kernel panicked".to_string(),
            GuestDeath::Silent(d) => format!(
                "guest console silent for {}s (hung, or a panic whose text was garbled)",
                d.as_secs()
            ),
            GuestDeath::Livelocked(d) => format!(
                "guest printed nothing new for {}s while flooding the console (livelock)",
                d.as_secs()
            ),
        }
    }

    /// Process exit code, kept distinct from 33 (tests reported failures) and 34 (timed out).
    pub fn exit_code(&self) -> i32 {
        match self {
            GuestDeath::Panicked => 35,
            GuestDeath::Silent(_) => 36,
            GuestDeath::Livelocked(_) => 37,
        }
    }
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

/// The KVM flags, shared by every subcommand that boots qemu.
#[derive(Args, Debug, Clone, Default)]
pub struct KvmOptions {
    #[clap(
        long,
        conflicts_with = "disable_kvm",
        help = "Force KVM acceleration on. Default: use KVM if the host supports it."
    )]
    enable_kvm: bool,
    #[clap(
        long,
        help = "Force KVM acceleration off. Default: use KVM if the host supports it."
    )]
    disable_kvm: bool,
}

impl KvmOptions {
    pub fn mode(&self) -> KvmMode {
        match (self.enable_kvm, self.disable_kvm) {
            (true, _) => KvmMode::Enabled,
            (_, true) => KvmMode::Disabled,
            _ => KvmMode::Auto,
        }
    }
}

/// Whether to run the guest under KVM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KvmMode {
    /// Use KVM if the host can support it.
    Auto,
    /// Use KVM regardless of what we detect.
    Enabled,
    /// Never use KVM.
    Disabled,
}

impl KvmMode {
    /// Resolve to an actual decision for a guest of `arch`. KVM can only run a guest of the host's
    /// own architecture, and needs /dev/kvm present.
    fn resolve(self, arch: Arch) -> bool {
        match self {
            KvmMode::Enabled => true,
            KvmMode::Disabled => false,
            KvmMode::Auto => {
                std::env::consts::ARCH == arch.to_string() && Path::new("/dev/kvm").exists()
            }
        }
    }
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
        self.arch_config(options.kvm.mode());

        // Connect disk image
        self.cmd.arg("-drive").arg(format!(
            "format=raw,file={}",
            disk_image.as_path().display()
        ));

        // qemu takes a write lock on this, so two runs sharing one image is a hard conflict, not a
        // race that usually works. `--disk-image` is how concurrent runs each get a private copy;
        // the shared per-triple image stays the default for interactive development.
        let disk_image_path = match &options.disk_image {
            Some(path) => {
                if !path.is_file() {
                    panic!(
                        "--disk-image {} does not exist; copy one from target/disk-<triple>.img \
                         (creating a fresh one here would lack the built /sysroot/pkg contents)",
                        path.display()
                    );
                }
                path.display().to_string()
            }
            None => {
                let path = format!(
                    "target/disk-{}.img",
                    options.config.twz_triple().to_string()
                );
                if !std::fs::exists(&path).unwrap() {
                    crate::disk::create_fresh_disk_image(&options.config.twz_triple()).unwrap();
                }
                path
            }
        };

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

        // Probe for a free port by binding it and letting it go; qemu binds it moments later. That
        // gap is only safe because concurrent runs are given distinct ports (`--ssh-port`) rather
        // than all racing for the default.
        let port = {
            let dynamic = || {
                TcpListener::bind("0.0.0.0:0").unwrap_or_else(|e| {
                    panic!("Port allocation for Qemu failed! {}", e);
                })
            };
            let listener = if options.ssh_port == 0 {
                dynamic()
            } else {
                TcpListener::bind(format!("0.0.0.0:{}", options.ssh_port)).unwrap_or_else(|_| {
                    println!(
                        "Failed to allocate port {} on host, dynamically assigning.",
                        options.ssh_port
                    );
                    dynamic()
                })
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

    fn arch_config(&mut self, kvm: KvmMode) {
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

                if kvm.resolve(self.arch) {
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
    println!(
        "{:<width$}  {:>8}  status",
        "test",
        "time",
        width = name_width
    );
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

/// Markers that mean the guest kernel is dead. `panic.rs` prints `[error] panicked at ...` first,
/// then the lock dump, the backtrace, and finally the test-mode banner. The banner claims the
/// machine is resetting, but the handler only halts the panicking cpu, so nothing ever resets it.
///
/// This is a *fast path only*, and an unreliable one: the emergency console cannot take a lock (it
/// has to be callable from critical sections and from the panic handler itself), so on a
/// multi-cpu panic these lines come out interleaved character by character with whatever another
/// cpu is printing, and no contiguous substring survives. An observed smp4 panic produced
/// `!!k! eTErSTn MeODlE /PAsNIrC c--/ RmESeETmTIoNGr`, matching neither marker. The silence
/// watchdog below is the mechanism that actually has to be correct.
const GUEST_DEAD_MARKERS: [&str; 2] = ["[error] panicked at", "TEST MODE PANIC"];

/// How long to keep draining serial output after a marker matches, so the lock dump and backtrace
/// that follow the first panic line still make it into the transcript.
const PANIC_DRAIN: Duration = Duration::from_secs(30);

/// How long the serial console may stay completely silent before we call the run dead.
///
/// A halted guest emits nothing, which is the one signal that cannot be garbled by interleaving.
/// This catches panics whose text was shredded, and equally catches guests that wedge without
/// panicking at all. Override with `TWZ_SILENCE_TIMEOUT` (seconds) if a genuinely quiet phase
/// trips it; `0` disables it.
fn silence_timeout() -> Option<Duration> {
    let secs = std::env::var("TWZ_SILENCE_TIMEOUT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(180u64);
    (secs > 0).then(|| Duration::from_secs(secs))
}

/// How long the guest may print without printing anything *new* before we call it livelocked.
///
/// The silence watchdog is blind to a guest that wedges while still logging -- an observed one grew
/// a 21MB transcript of one repeated fault line, and a later one burned a full 15-minute budget
/// repeating `failed to lock lock tracker`. Deliberately generous, and paired with the flood
/// threshold below, because a slow-but-working guest legitimately repeats a line for a while.
/// Override with `TWZ_PROGRESS_TIMEOUT` (seconds); `0` disables it.
fn progress_timeout() -> Option<Duration> {
    let secs = std::env::var("TWZ_PROGRESS_TIMEOUT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(600u64);
    (secs > 0).then(|| Duration::from_secs(secs))
}

/// Repeated lines needed within the progress window before repetition counts as a livelock rather
/// than as a quiet phase.
///
/// Sized against both observed livelocks, which differ by four orders of magnitude in rate: Mode B
/// produced 21MB of one line, while the tracker-lock hang repeats *slowly* -- session 7's sighting
/// was 164 repeats across a whole 15-minute budget, and session 8 caught two more at ~17 repeats
/// over several minutes. A threshold sized off the flooding case would miss the slow one entirely,
/// which is the one that has actually been costing budgets. The staleness window does the real
/// work; this only has to exceed what a healthy run repeats, and a healthy `debug-nokvm-smp4`
/// transcript repeats `REPORT {"status":"Pending"}` 62 times *in total*, at most 28 consecutively.
const FLOOD_LINES: u64 = 64;

/// Distinct recent lines remembered when deciding whether a line is new. Large enough that a short
/// cycle of a few repeating messages is still recognized as repetition, small enough that a run's
/// whole history is not held in memory.
const NOVELTY_WINDOW: usize = 512;

/// Shared progress state: when the guest last said something new, and how many lines it has
/// repeated since. Cloned into the reader thread and polled by the waiter.
#[derive(Clone)]
struct Progress {
    last_new: Arc<AtomicU64>,
    repeats: Arc<AtomicU64>,
}

/// Rolling record of what the guest has said lately, so "still printing" can be distinguished from
/// "still making progress".
struct Novelty {
    recent: std::collections::VecDeque<u64>,
    seen: std::collections::HashSet<u64>,
}

impl Novelty {
    fn new() -> Self {
        Self {
            recent: std::collections::VecDeque::with_capacity(NOVELTY_WINDOW),
            seen: std::collections::HashSet::with_capacity(NOVELTY_WINDOW),
        }
    }

    /// Record a line, returning true if it is not one of the last `NOVELTY_WINDOW` distinct lines.
    fn is_new(&mut self, line: &str) -> bool {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        line.hash(&mut h);
        let key = h.finish();
        if !self.seen.insert(key) {
            return false;
        }
        self.recent.push_back(key);
        if self.recent.len() > NOVELTY_WINDOW {
            if let Some(old) = self.recent.pop_front() {
                self.seen.remove(&old);
            }
        }
        true
    }
}

/// How long to keep reading after asking the monitor for cpu state, so its reply reaches the
/// transcript before we kill qemu.
const MONITOR_DRAIN: Duration = Duration::from_secs(5);

/// How long to let the register dump arrive before we read the stack pointers out of it.
const REGISTER_DRAIN: Duration = Duration::from_secs(2);

/// Stack words to read per cpu. Enough to cover several frames of kernel stack without burying the
/// transcript.
const STACK_WORDS: usize = 64;

/// Pull the value out of every `RSP=<hex>` on a line of monitor output.
///
/// The stack has to be asked for by literal address: HMP rejects `x/64gx $rsp` with "unknown
/// register" on this target, so the address comes from parsing `info registers -a` instead of from
/// an expression qemu evaluates for us.
fn scrape_rsp(line: &str) -> Option<u64> {
    let rest = line.split("RSP=").nth(1)?;
    let hex: String = rest.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
    u64::from_str_radix(&hex, 16).ok()
}

/// Ask qemu's monitor what every vcpu is doing, and let the reply land in the transcript.
///
/// This is the only diagnostic that survives *any* wedge, because it needs no cooperation from the
/// guest: it works when a cpu is spinning with interrupts masked, when the console lock is held,
/// and when every cpu is halted waiting on a wakeup that never came. Those three are exactly what a
/// silent hang cannot otherwise distinguish, and `info registers -a` separates them at a glance --
/// all cpus halted means a lost wakeup, one cpu with a kernel RIP means a spin, and the RIP names
/// the code. Symbolize with
/// `addr2line -fe target/kernel/x86_64-unknown-none/<profile>/twizzler-kernel <rip>`.
///
/// `-serial mon:stdio` multiplexes the monitor onto the same stdio as the guest console, so getting
/// at it means sending the mux escape (Ctrl-A c) to move input focus first. Output is captured
/// without extra work: the reader thread is already streaming everything qemu writes.
///
/// Done in two passes, because the stack has to be asked for by literal address: the first pass
/// dumps registers, the reader thread scrapes the stack pointers out of it, and the second pass
/// reads each one. Stacks matter because RIP on its own is often not enough -- a spinning cpu's RIP
/// lands in a spin helper, which says that it is spinning without saying who asked it to, and the
/// return addresses still on the stack are what name the caller.
fn dump_guest_state(stdin: &mut std::process::ChildStdin, rsps: &Mutex<Vec<u64>>) {
    const MUX_TO_MONITOR: &[u8] = b"\x01c";
    rsps.lock().map(|mut r| r.clear()).ok();
    let _ = stdin.write_all(MUX_TO_MONITOR);
    let _ = stdin.write_all(b"\ninfo registers -a\n");
    let _ = stdin.flush();
    std::thread::sleep(REGISTER_DRAIN);

    let stacks: Vec<u64> = rsps.lock().map(|r| r.clone()).unwrap_or_default();
    for rsp in stacks {
        let _ = stdin.write_all(format!("x/{}gx 0x{:x}\n", STACK_WORDS, rsp).as_bytes());
    }
    let _ = stdin.flush();
    std::thread::sleep(MONITOR_DRAIN);
}

/// Stream qemu's serial output to our stdout and, if we have one, to a transcript file, returning
/// the first complete test report seen.
///
/// `panicked` is raised as soon as a death marker goes by, and `last_line` is bumped on every line,
/// so the waiter can stop a run that has died instead of waiting out the heartbeat budget.
fn read_serial(
    stdout: Option<std::process::ChildStdout>,
    mut log: Option<std::fs::File>,
    panicked: Arc<AtomicBool>,
    last_line: Arc<AtomicU64>,
    progress: Progress,
    rsps: Arc<Mutex<Vec<u64>>>,
    started: std::time::Instant,
) -> Option<ReportInfo> {
    let stdout = stdout?;
    let mut found = None;
    let mut novelty = Novelty::new();
    for line in BufReader::new(stdout).lines() {
        last_line.store(started.elapsed().as_millis() as u64, Ordering::SeqCst);
        let Ok(line) = line else {
            continue;
        };
        let line = line.trim();
        if novelty.is_new(line) {
            progress
                .last_new
                .store(started.elapsed().as_millis() as u64, Ordering::SeqCst);
            progress.repeats.store(0, Ordering::SeqCst);
        } else {
            progress.repeats.fetch_add(1, Ordering::SeqCst);
        }
        println!(" ==> {}", line);
        // Monitor register dumps are the only thing that prints RSP=, and dump_guest_state
        // clears this immediately before asking, so whatever lands here is the current dump.
        if let Some(rsp) = scrape_rsp(line) {
            let _ = rsps.lock().map(|mut r| r.push(rsp));
        }
        if let Some(log) = log.as_mut() {
            let _ = writeln!(log, "{}", line);
        }
        if GUEST_DEAD_MARKERS.iter().any(|m| line.contains(m)) {
            panicked.store(true, Ordering::SeqCst);
        }
        // Keep draining once a report has landed rather than dropping the pipe: anything the
        // guest prints afterwards would otherwise fill it and wedge qemu.
        if found.is_none() {
            if let Some(rest) = line.strip_prefix("REPORT ") {
                if let Ok(ReportStatus::Ready(report)) =
                    unittest_report::Report::from_prefix(rest.trim()).map(|report| report.status)
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

    let serial_log = run.monitor.then(|| {
        run.serial_log
            .clone()
            .unwrap_or_else(|| serial_log_path(&run.label))
    });
    let log_file = serial_log.as_ref().and_then(|path| {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        std::fs::File::create(path)
            .inspect_err(|e| eprintln!("failed to open serial log {}: {}", path.display(), e))
            .ok()
    });
    let logging_to = serial_log.as_ref().filter(|_| log_file.is_some()).cloned();

    let panicked = Arc::new(AtomicBool::new(false));
    let last_line = Arc::new(AtomicU64::new(0));
    let started = std::time::Instant::now();
    let rsps: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
    let progress = Progress {
        last_new: Arc::new(AtomicU64::new(0)),
        repeats: Arc::new(AtomicU64::new(0)),
    };
    let reader_panicked = panicked.clone();
    let reader_last_line = last_line.clone();
    let reader_rsps = rsps.clone();
    let reader_progress = progress.clone();
    let reader_thread = std::thread::spawn(move || {
        read_serial(
            child_stdout,
            log_file,
            reader_panicked,
            reader_last_line,
            reader_progress,
            reader_rsps,
            started,
        )
    });

    let mut death: Option<GuestDeath> = None;
    let exit_status = if run.monitor {
        // Poll at a finer grain than the heartbeat so a dead guest is noticed promptly, but keep
        // counting tries in heartbeat-sized units: `heartbeat_tries` is the run's wall-clock budget
        // and callers tune it per configuration.
        const POLL: Duration = Duration::from_secs(1);
        const HEARTBEAT: Duration = Duration::from_secs(15);
        let silence_timeout = silence_timeout();
        let progress_timeout = progress_timeout();
        let mut tries = 0;
        let mut since_heartbeat = Duration::ZERO;
        let mut died_at: Option<std::time::Instant> = None;
        loop {
            if let Some(es) = child.wait_timeout(POLL)? {
                break Some(es);
            }

            // A guest that has stopped talking has stopped running. This is the check that has to
            // work, because the textual markers do not survive concurrent cpus printing.
            if let Some(limit) = silence_timeout {
                let quiet = started
                    .elapsed()
                    .saturating_sub(Duration::from_millis(last_line.load(Ordering::SeqCst)));
                if quiet >= limit {
                    death = Some(GuestDeath::Silent(quiet));
                    if let Some(stdin) = child_stdin.as_mut() {
                        dump_guest_state(stdin, &rsps);
                    }
                    break None;
                }
            }

            // A guest can wedge without going quiet: it repeats one fault or one warning forever.
            // Requiring a flood as well as staleness keeps a slow phase that legitimately repeats a
            // line -- a pending report, say -- from being called dead.
            if let Some(limit) = progress_timeout {
                let stale = started.elapsed().saturating_sub(Duration::from_millis(
                    progress.last_new.load(Ordering::SeqCst),
                ));
                if stale >= limit && progress.repeats.load(Ordering::SeqCst) >= FLOOD_LINES {
                    death = Some(GuestDeath::Livelocked(stale));
                    if let Some(stdin) = child_stdin.as_mut() {
                        dump_guest_state(stdin, &rsps);
                    }
                    break None;
                }
            }

            if panicked.load(Ordering::SeqCst) {
                // Give the panic path time to finish spilling its lock dump and backtrace, then
                // stop: it is never coming back on its own.
                death = Some(GuestDeath::Panicked);
                let died_at = *died_at.get_or_insert_with(std::time::Instant::now);
                if died_at.elapsed() >= PANIC_DRAIN {
                    break None;
                }
                continue;
            }

            since_heartbeat += POLL;
            if since_heartbeat < HEARTBEAT {
                continue;
            }
            since_heartbeat = Duration::ZERO;

            let Some(stdin) = child_stdin.as_mut() else {
                break None;
            };
            if stdin.write_all(b"status\n").is_err() {
                // qemu closed its end, so it is already on its way out; collect its status
                // instead of calling this a timeout.
                break child.wait_timeout(HEARTBEAT)?;
            }
            tries += 1;
            if tries > run.heartbeat_tries {
                // The budget can run out on a guest that is wedged but not silent (a livelock that
                // keeps logging), and on one that is silent but whose silence budget is the larger
                // of the two -- which is how the `pager ready` hang currently ends. Same evidence
                // is wanted either way.
                dump_guest_state(stdin, &rsps);
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
        guest_death: death,
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

    if let Some(death) = outcome.guest_death {
        eprintln!("FAILED: {}", death.describe());
        std::process::exit(death.exit_code());
    }

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
