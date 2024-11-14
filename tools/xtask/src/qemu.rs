use std::{
    fs::OpenOptions,
    path::Path,
    process::{Command, ExitStatus},
};

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
        self.cmd.arg("-m").arg("2048,slots=4,maxmem=8G");

        // configure architechture specific parameters
        self.arch_config(options);

        // Connect disk image
        self.cmd.arg("-drive").arg(format!(
            "format=raw,file={}",
            image_info.disk_image.as_path().display()
        ));

        if let Ok(f) = OpenOptions::new()
            .write(true)
            .create(true)
            .open("target/nvme.img")
        {
            f.set_len(0x1000000).unwrap();
        }

        self.cmd
            .arg("-drive")
            .arg("file=target/nvme.img,if=none,id=nvme")
            .arg("-device")
            .arg("nvme,serial=deadbeef,drive=nvme");

        self.cmd
            .arg("--no-reboot") // exit instead of rebooting
            .arg("-s") // shorthand for -gdb tcp::1234
            // .arg("-serial")
            // .arg("mon:stdio")
            .arg("-nographic")
            .arg("-smp")
            .arg("4");
        self.cmd.arg("-serial").arg("mon:stdio");

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
                if options.tests || options.benches {
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

    pub fn status(&mut self) -> std::io::Result<ExitStatus> {
        self.cmd.status()
    }
}

pub(crate) fn do_start_qemu(cli: QemuOptions) -> anyhow::Result<()> {
    let image_info = crate::image::do_make_image((&cli).into())?;

    let mut run_cmd = QemuCommand::new(&cli);
    run_cmd.config(&cli, image_info);

    let exit_status = run_cmd.status()?;
    if exit_status.success() {
        Ok(())
    } else {
        if cli.tests || cli.benches {
            if exit_status.code().unwrap() == 1 {
                eprintln!("qemu reports tests passed");
                std::process::exit(0);
            } else {
                eprintln!("qemu reports tests failed");
                std::process::exit(33);
            }
        }
        anyhow::bail!("qemu return with error");
    }
}
