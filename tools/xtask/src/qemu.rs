use std::{
    fs::OpenOptions,
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    path::Path,
    process::{Command, Stdio},
    str::FromStr,
    time::Duration,
};

use unittest_report::ReportStatus;

use crate::{
    image::ImageInfo,
    triple::{Arch, Machine},
    QemuOptions,
};

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

    pub fn config(&mut self, options: &QemuOptions, image_info: ImageInfo) {
        // Set up the basic stuff, memory and bios, etc.
        self.cmd.arg("-m").arg("4096,slots=4,maxmem=8G");

        // configure architechture specific parameters
        self.arch_config(options);

        // Connect disk image
        self.cmd.arg("-drive").arg(format!(
            "format=raw,file={}",
            image_info.disk_image.as_path().display()
        ));

        let already_exists = std::fs::exists("target/nvme.img").unwrap();
        if let Ok(f) = OpenOptions::new()
            .write(true)
            .create(true)
            .open("target/nvme.img")
        {
            f.set_len(1024 * 1024 * 1024 * 100).unwrap();
        }

        std::env::set_var(
            "PATH",
            format!(
                "{}:{}",
                std::env::var("PATH").unwrap(),
                "/opt/homebrew/opt/e2fsprogs/sbin/"
            ),
        );
        if !already_exists {
            if !Command::new("mke2fs")
                .arg("-b")
                .arg("4096")
                .arg("-qF")
                .arg("-E")
                .arg("test_fs")
                .arg("target/nvme.img")
                .arg("10000000")
                .status()
                .expect("failed to create disk image")
                .success()
            {
                panic!("failed to run mke2fs on nvme.img");
            }
        }

        self.cmd
            .arg("-drive")
            .arg("file=target/nvme.img,if=none,id=nvme")
            .arg("-device")
            .arg("nvme,serial=deadbeef,drive=nvme");

        self.cmd.arg("-device").arg("virtio-net-pci,netdev=net0");

        let port = {
            let listener = match TcpListener::bind("0.0.0.0:5555") {
                Ok(l) => l,
                Err(_) => {
                    println!(
                        "Failed to allocate default port 5555 on host, dynamically assigning."
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

        self.cmd
            .arg("-netdev")
            .arg(format!("user,id=net0,hostfwd=tcp::{}-:5555", port));

        self.cmd
            .arg("--no-reboot") // exit instead of rebooting
            //.arg("-s") // shorthand for -gdb tcp::1234
            .arg("-serial")
            .arg("mon:stdio");
        //-serial mon:stdio creates a multiplexed stdio backend connected
        // to the serial port and the QEMU monitor, and
        // -nographic also multiplexes the console and the monitor to stdio.

        // add additional options for qemu
        self.cmd.args(&options.qemu_options);

        //self.cmd.arg("-smp").arg("4,sockets=1,cores=2,threads=2");
    }

    fn arch_config(&mut self, options: &QemuOptions) {
        match self.arch {
            Arch::X86_64 => {
                // bios, platform
                self.cmd.arg("-bios").arg("toolchain/install/OVMF.fd");
                self.cmd.arg("-machine").arg("q35,nvdimm=on");

                // add qemu exit device for testing
                if options.tests || options.benches || options.bench.is_some() {
                    // x86 specific
                    self.cmd
                        .arg("-device")
                        .arg("isa-debug-exit,iobase=0xf4,iosize=0x04");
                }

                let has_kvm = std::env::consts::ARCH == self.arch.to_string()
                    && Path::new("/dev/kvm").exists();

                if has_kvm {
                    self.cmd.arg("-enable-kvm");
                    self.cmd
                        .arg("-cpu")
                        .arg("host,+x2apic,+tsc-deadline,+invtsc,+tsc,+tsc_scale,+rdtscp");
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
                self.cmd.arg("-bios").arg("toolchain/install/OVMF-AA64.fd");
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

pub(crate) fn do_start_qemu(cli: QemuOptions) -> anyhow::Result<()> {
    let image_info = crate::image::do_make_image((&cli).into())?;

    let mut run_cmd = QemuCommand::new(&cli);
    run_cmd.config(&cli, image_info);

    use wait_timeout::ChildExt;
    let timeout = cli.tests;
    let heartbeat = cli.tests;
    if heartbeat {
        run_cmd.cmd.stdin(Stdio::piped());
        run_cmd.cmd.stdout(Stdio::piped());
    }

    let mut child = run_cmd.cmd.spawn()?;

    let mut child_stdin = child.stdin.take();
    let child_stdout = child.stdout.take();

    let reader_thread = std::thread::spawn(|| {
        if let Some(child_stdout) = child_stdout {
            let reader = BufReader::new(child_stdout);
            let mut ret = None;
            for line in reader.lines().into_iter() {
                if let Ok(line) = line {
                    println!(" ==> {}", line.trim());
                    if line.trim().starts_with("REPORT ") {
                        let line = line.trim().strip_prefix("REPORT ").unwrap();
                        let report = unittest_report::Report::from_str(line.trim());
                        if let Ok(ReportStatus::Ready(report)) = report.map(|report| report.status)
                        {
                            ret = Some(report);
                            break;
                        }
                    }
                }
            }
            ret
        } else {
            None
        }
    });

    let exit_status = if timeout {
        if heartbeat {
            let mut i = 0;
            loop {
                if let Some(es) = child.wait_timeout(Duration::from_secs(10))? {
                    break Some(es);
                }
                child_stdin
                    .as_mut()
                    .unwrap()
                    .write_all(b"status\n")
                    .unwrap();
                i += 1;
                if i > 10 {
                    break None;
                }
            }
        } else {
            child.wait_timeout(Duration::from_secs(60))?
        }
    } else {
        Some(child.wait()?)
    };

    let Some(exit_status) = exit_status else {
        eprintln!("qemu timed out");
        child.kill().unwrap();
        std::process::exit(34);
    };

    let report = reader_thread.join().ok().flatten();
    if let Some(report) = report {
        let successes = report.tests.iter().filter(|t| t.passed).count();
        let total = report.tests.len();
        println!(
            "TEST RESULTS: {} passed, {} failed, {} total -- time: {:2} seconds",
            successes,
            total - successes,
            total,
            report.time.as_millis() as f64 / 1000.0,
        );
    } else if cli.tests {
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
