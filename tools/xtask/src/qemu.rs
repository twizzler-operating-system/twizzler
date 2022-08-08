use std::{fs::File, process::Command};

use crate::QemuOptions;

pub(crate) fn do_start_qemu(cli: QemuOptions) -> anyhow::Result<()> {
    let image_info = crate::image::do_make_image((&cli).into())?;

    let mut run_cmd = Command::new("qemu-system-x86_64");

    // Set up the basic stuff, memory and bios, etc.
    run_cmd.arg("-m").arg("1024,slots=4,maxmem=8G");
    run_cmd.arg("-enable-kvm");
    run_cmd.arg("-bios").arg("toolchain/install/OVMF.fd");

    run_cmd.arg("-machine").arg("q35,nvdimm=on");
    run_cmd
        .arg("-cpu")
        .arg("host,+x2apic,+tsc-deadline,+invtsc,+tsc,+tsc_scale");

    // Connect disk image
    run_cmd.arg("-drive").arg(format!(
        "format=raw,file={}",
        image_info.disk_image.as_path().display()
    ));
    // Connect some nvdimms
    /*
    run_cmd.arg("-object").arg(format!(
        "memory-backend-file,id=mem1,share=on,mem-path={},size=4G",
        make_path(build_info, true, "pmem.img")
    ));
    run_cmd.arg("-device").arg("nvdimm,id=nvdimm1,memdev=mem1");
    */
    File::create("target/nvme.img")
        .and_then(|f| f.set_len(0x10000000))
        .unwrap();
    run_cmd
        .arg("-drive")
        .arg("file=target/nvme.img,if=none,id=nvme")
        .arg("-device")
        .arg("nvme,serial=deadbeef,drive=nvme");
    run_cmd
        .arg("--no-reboot")
        .arg("-s")
        .arg("-serial")
        .arg("mon:stdio");

    if cli.tests {
        run_cmd
            .arg("-device")
            .arg("isa-debug-exit,iobase=0xf4,iosize=0x04");
    }
    run_cmd.args(cli.qemu_options);
    //run_cmd.arg("-smp").arg("4,sockets=1,cores=2,threads=2");

    let exit_status = run_cmd.status()?;
    if exit_status.success() {
        Ok(())
    } else {
        if cli.tests {
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
