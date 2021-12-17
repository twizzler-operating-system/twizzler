use std::{env, process::Command, str::FromStr, vec};

use cargo_metadata::{Metadata, MetadataCommand};

type DynError = Box<dyn std::error::Error>;

fn main() {
    if let Err(e) = try_main() {
        eprintln!("{}", e);
        std::process::exit(101);
    }
}

#[derive(Copy, Clone, Debug, PartialEq)]
enum Profile {
    Debug,
    Release,
}

#[derive(Copy, Clone, Debug, PartialEq)]
enum QemuProfile {
    Accel,
    Emu,
}

impl QemuProfile {
    fn get_args(&self) -> Vec<&str> {
        match *self {
            QemuProfile::Accel => vec![
                "-enable-kvm",
                "-cpu",
                "host,+x2apic,+tsc-deadline,+invtsc,+tsc,+tsc_scale",
            ],
            QemuProfile::Emu => vec!["-cpu", "IvyBridge"],
        }
    }
}

use clap::{App, Arg, SubCommand};
fn try_main() -> Result<(), DynError> {
    let arg_profile = Arg::with_name("profile")
        .long("profile")
        .takes_value(true)
        .default_value("debug")
        .possible_values(&["debug", "release"])
        .help("Set build profile");

    let arg_qemu = Arg::with_name("qemu-arg")
        .long("qemu-arg")
        .multiple(true)
        .takes_value(true)
        .help("Argument to pass straight through to QEMU");

    let arg_qemu_profile = Arg::with_name("qemu-profile")
        .long("qemu-profile")
        .takes_value(true)
        .default_value("accel")
        .possible_values(&["accel", "emu"])
        .help("Sets the QEMU base configuration");

    let build = SubCommand::with_name("build-all").arg(arg_profile.clone());
    let check = SubCommand::with_name("check-all").arg(arg_profile.clone());
    let disk = SubCommand::with_name("make-disk").arg(arg_profile.clone());
    let qemu = SubCommand::with_name("start-qemu")
        .arg(arg_profile.clone())
        .arg(arg_qemu_profile)
        .arg(arg_qemu);

    let app = App::new("twizzler-xtask")
        .version("0.1.0")
        .about("Build system for Twizzler")
        .subcommand(build)
        .subcommand(disk)
        .subcommand(qemu)
        .subcommand(check);
    let matches = app.get_matches();
    let (sub_name, sub_matches) = matches.subcommand();

    let sub_matches = sub_matches.unwrap();
    let profile = match sub_matches.value_of("profile").unwrap() {
        "debug" => Profile::Debug,
        "release" => Profile::Release,
        _ => unreachable!(),
    };

    let qemu_profile = match sub_matches.value_of("qemu-profile") {
        Some("accel") => QemuProfile::Accel,
        Some("emu") => QemuProfile::Emu,
        None => QemuProfile::Accel,
        _ => unreachable!(),
    };

    let path = "Cargo.toml";
    let meta = MetadataCommand::new().manifest_path(path).exec().unwrap();
    let mut args = vec![];
    if profile == Profile::Release {
        args.push("--release".to_owned());
    }
    let mut qemu_args = vec![];
    if let Some(q) = sub_matches.values_of("qemu-arg") {
        for item in q {
            qemu_args.push(item.to_owned());
        }
    }
    match sub_name {
        "build-all" => build_all(&meta, &args, profile)?,
        "check-all" => check_all(&meta, &args, profile)?,
        "make-disk" => make_disk(&meta, &args, profile)?,
        "start-qemu" => start_qemu(&meta, &args, profile, qemu_profile, &qemu_args)?,
        _ => unreachable!(),
    }
    Ok(())
}

fn cargo_cmd_collection(
    meta: &Metadata,
    collection_name: &str,
    cargo_cmd: &str,
    wd: &str,
    args: &[String],
    profile: Profile,
) -> Result<(), DynError> {
    eprintln!(
        "== BUILDING COLLECTION {} ({:?}) ==",
        collection_name, profile
    );
    let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let pkg_list: Vec<String> = meta.workspace_metadata[collection_name]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| {
            [
                String::from_str("--bin").unwrap(),
                x.to_string().replace("\"", ""),
            ]
        })
        .flatten()
        .collect();
    //println!("{:?}", pkg_list);
    let status = Command::new(cargo)
        .current_dir(wd)
        .arg(cargo_cmd)
        .args(pkg_list)
        .args(args)
        .status()?;

    if !status.success() {
        Err("failed to run cargo command")?;
    }
    Ok(())
}

fn cmd_all(
    meta: &Metadata,
    args: &[String],
    cargo_cmd: &str,
    profile: Profile,
) -> Result<(), DynError> {
    cargo_cmd_collection(meta, "tools", cargo_cmd, ".", args, profile)?;
    cargo_cmd_collection(meta, "kernel", cargo_cmd, "src/kernel", args, profile)?;
    cargo_cmd_collection(meta, "initrd-members", cargo_cmd, ".", args, profile)?;
    Ok(())
}

fn check_all(meta: &Metadata, args: &[String], profile: Profile) -> Result<(), DynError> {
    cmd_all(meta, args, "check", profile)?;
    Ok(())
}

fn build_all(meta: &Metadata, args: &[String], profile: Profile) -> Result<(), DynError> {
    cmd_all(meta, args, "build", profile)?;
    Ok(())
}

fn make_disk(meta: &Metadata, args: &[String], profile: Profile) -> Result<(), DynError> {
    build_all(meta, args, profile)?;
    let profile_path = match profile {
        Profile::Debug => "debug",
        Profile::Release => "release",
    };
    eprintln!("== BUILDING DISK IMAGE ({:?}) ==", profile);
    let status = Command::new(format!("target/{}/image_builder", profile_path))
        .arg(format!(
            "target/x86_64-pc-none/{}/twizzler-kernel",
            profile_path
        ))
        .status()?;

    if !status.success() {
        Err("disk image creation failed")?;
    }
    Ok(())
}

fn start_qemu(
    meta: &Metadata,
    args: &[String],
    profile: Profile,
    qemu_profile: QemuProfile,
    qemu_args: &[String],
) -> Result<(), DynError> {
    make_disk(meta, args, profile)?;
    let profile_path = match profile {
        Profile::Debug => "debug",
        Profile::Release => "release",
    };
    let mut run_cmd = Command::new("qemu-system-x86_64");
    run_cmd.arg("-m").arg("1024,slots=4,maxmem=8G");
    run_cmd.arg("-bios").arg("/usr/share/edk2-ovmf/x64/OVMF.fd");
    run_cmd.arg("-smp").arg("4,sockets=1,cores=2,threads=2");
    run_cmd.arg("-drive").arg(format!(
        "format=raw,file=target/x86_64-pc-none/{}/disk.img",
        profile_path
    ));
    run_cmd.arg("-machine").arg("q35,nvdimm=on");
    run_cmd
        .arg("-object")
        .arg("memory-backend-file,id=mem1,share=on,mem-path=pmem.img,size=4G");
    run_cmd.arg("-device").arg("nvdimm,id=nvdimm1,memdev=mem1");
    const RUN_ARGS: &[&str] = &["--no-reboot", "-s", "-serial", "mon:stdio", "-vnc", ":0"];
    run_cmd.args(RUN_ARGS);
    run_cmd.args(qemu_profile.get_args());
    run_cmd.args(qemu_args);

    let exit_status = run_cmd.status().unwrap();
    if !exit_status.success() {
        Err("failed to run qemu")?;
    }
    Ok(())
}
