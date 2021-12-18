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

    let arg_workspace = Arg::with_name("workspace").long("workspace").hidden(true);
    let arg_manifest = Arg::with_name("manifest-path")
        .long("manifest-path")
        .hidden(true)
        .takes_value(true);
    let arg_message_fmt = Arg::with_name("message-format")
        .long("message-format")
        .takes_value(true)
        .hidden(true);

    let arg_qemu_profile = Arg::with_name("qemu-profile")
        .long("qemu-profile")
        .takes_value(true)
        .default_value("accel")
        .possible_values(&["accel", "emu"])
        .help("Sets the QEMU base configuration");

    let bootstrap_sc = SubCommand::with_name("bootstrap")
        .about("Bootstrap the rust toolchain for Twizzler")
        .arg(
            Arg::with_name("skip-submodules")
                .long("skip-submodules")
                .help("Skips updating submodules before compiling rust toolchain"),
        );
    let build = SubCommand::with_name("build-all")
        .arg(arg_profile.clone())
        .arg(arg_message_fmt.clone())
        .arg(arg_workspace.clone())
        .about("Run cargo build on all Twizzler components");
    let check = SubCommand::with_name("check-all")
        .arg(arg_profile.clone())
        .arg(arg_message_fmt)
        .arg(arg_workspace)
        .arg(arg_manifest)
        .about("Run cargo check on all Twizzler components");
    let disk = SubCommand::with_name("make-disk")
        .arg(arg_profile.clone())
        .about("Create disk image from compiled Twizzler components");
    let qemu = SubCommand::with_name("start-qemu")
        .about("Start QEMU instance using the disk image created with twizzler-xtask-make-disk")
        .arg(arg_profile.clone())
        .arg(arg_qemu_profile)
        .arg(arg_qemu);

    let mut app = App::new("twizzler-xtask")
        .version("0.1.0")
        .about("Build system for Twizzler. This program correctly applies the right compiling rules (e.g. target, RUSTFLAGS, etc.) to Twizzler components.")
        .author("Daniel Bittman <danielbittman1@gmail.com>")
        .subcommand(bootstrap_sc)
        .subcommand(build)
        .subcommand(disk)
        .subcommand(qemu)
        .subcommand(check);
    let matches = app.clone().get_matches();
    let (sub_name, sub_matches) = matches.subcommand();

    if sub_matches.is_none() {
        app.print_long_help()?;
        Err("")?;
    }

    let sub_matches = sub_matches.unwrap();
    let profile = match sub_matches.value_of("profile") {
        Some("debug") => Profile::Debug,
        Some("release") => Profile::Release,
        None => Profile::Debug,
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
    let mut args = vec!["--workspace".to_owned()];
    if profile == Profile::Release {
        args.push("--release".to_owned());
    }
    if let Some(v) = sub_matches.value_of("message-format") {
        args.push(format!("--message-format={}", v));
    }
    if let Some(v) = sub_matches.value_of("manifest-path") {
        args.push("--manifest-path".to_owned());
        args.push(v.to_owned());
    }
    let mut qemu_args = vec![];
    if let Some(q) = sub_matches.values_of("qemu-arg") {
        for item in q {
            qemu_args.push(item.to_owned());
        }
    }
    match sub_name {
        "bootstrap" => bootstrap(sub_matches.is_present("skip-submodules"))?,
        "build-all" => build_all(&meta, &args, profile)?,
        "check-all" => check_all(&meta, &args, profile)?,
        "make-disk" => make_disk(&meta, &args, profile)?,
        "start-qemu" => start_qemu(&meta, &args, profile, qemu_profile, &qemu_args)?,
        _ => unreachable!(),
    }
    Ok(())
}

fn bootstrap(skip_sm: bool) -> Result<(), DynError> {
    if !skip_sm {
        let status = Command::new("git")
            .arg("submodule")
            .arg("update")
            .arg("--init")
            .arg("--recursive")
            .status()?;
        if !status.success() {
            Err("failed to update git submodules")?;
        }
    }

    let res = std::fs::hard_link(
        "toolchain/src/config.toml",
        "toolchain/src/rust/config.toml",
    );
    match res {
        Err(e) => match e.kind() {
            std::io::ErrorKind::AlreadyExists => {}
            _ => Err("failed to create hardlink config.toml")?,
        },
        _ => {}
    }

    let status = Command::new("./x.py")
        .arg("install")
        .current_dir("toolchain/src/rust")
        .status()?;
    if !status.success() {
        Err("failed to compile rust toolchain")?;
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
    triple: Option<&str>,
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
    let mut target_args = vec![];
    if let Some(triple) = triple {
        target_args.push("--target".to_owned());
        target_args.push(triple.to_owned());
        target_args.push("-Zbuild-std".to_owned());
    }
    let status = Command::new(cargo)
        .current_dir(wd)
        .arg(cargo_cmd)
        .args(pkg_list)
        .args(target_args)
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
    cargo_cmd_collection(meta, "tools", cargo_cmd, ".", args, profile, None)?;
    cargo_cmd_collection(meta, "kernel", cargo_cmd, "src/kernel", args, profile, None)?;
    cargo_cmd_collection(
        meta,
        "twizzler-apps",
        cargo_cmd,
        ".",
        args,
        profile,
        Some("x86_64-unknown-linux-gnu"),
    )?;
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
