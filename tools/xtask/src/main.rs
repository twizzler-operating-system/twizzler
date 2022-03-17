use std::{env, fmt::Display, path::Path, process::Command, str::FromStr, vec};

use cargo_metadata::{Metadata, MetadataCommand};

type DynError = Box<dyn std::error::Error>;

use rand::Rng;
use std::io::{Read, Write};
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

impl Profile {
    fn as_str(&self) -> &str {
        match self {
            Self::Debug => "debug",
            Self::Release => "release",
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq)]
enum Arch {
    X86,
    Aarch64,
}

impl Arch {
    fn as_str(&self) -> &str {
        match self {
            Self::X86 => "x86_64",
            Self::Aarch64 => "aarch64",
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq)]
enum Platform {
    Unknown,
    Rpi3,
}

impl Platform {
    fn as_str(&self) -> &str {
        match self {
            Self::Unknown => "unknown",
            Self::Rpi3 => "rpi3",
        }
    }
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

#[derive(Clone, Copy, Debug, PartialEq)]
struct BuildInfo {
    profile: Profile,
    arch: Arch,
    platform: Platform,
}

impl Display for BuildInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}-{}::{}",
            self.arch.as_str(),
            self.platform.as_str(),
            self.profile.as_str()
        )
    }
}

impl BuildInfo {
    fn get_twizzler_triple(&self) -> String {
        format!("{}-{}-twizzler", self.arch.as_str(), self.platform.as_str())
    }

    fn get_kernel_triple(&self) -> String {
        format!("{}-{}-none", self.arch.as_str(), self.platform.as_str())
    }
}

fn all_supported_build_infos() -> Vec<BuildInfo> {
    vec![BuildInfo {
        profile: Profile::Debug,
        platform: Platform::Unknown,
        arch: Arch::X86,
    }]
}

use clap::{App, Arg, SubCommand};
use fs_extra::dir::CopyOptions;
fn try_main() -> Result<(), DynError> {
    let arg_profile = Arg::with_name("profile")
        .long("profile")
        .takes_value(true)
        .default_value("debug")
        .possible_values(&["debug", "release"])
        .help("Set build profile");

    let arg_arch = Arg::with_name("arch")
        .long("arch")
        .takes_value(true)
        .default_value("x86_64")
        .possible_values(&["x86_64", "aarch64"])
        .help("Set target architecture");

    let arg_platform = Arg::with_name("platform")
        .long("platform")
        .takes_value(true)
        .default_value("unknown")
        .possible_values(&["unknown", "rpi3"])
        .help("Set target platform");

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
    let build_std_sc =
        SubCommand::with_name("build-std").about("Recompile Rust libstd for Twizzler");
    let build = SubCommand::with_name("build-all")
        .arg(arg_profile.clone())
        .arg(arg_arch.clone())
        .arg(arg_platform.clone())
        .arg(arg_message_fmt.clone())
        .arg(arg_workspace.clone())
        .about("Run cargo build on all Twizzler components");
    let check = SubCommand::with_name("check-all")
        .arg(arg_profile.clone())
        .arg(arg_arch.clone())
        .arg(arg_platform.clone())
        .arg(arg_message_fmt)
        .arg(arg_workspace)
        .arg(arg_manifest)
        .about("Run cargo check on all Twizzler components");
    let disk = SubCommand::with_name("make-disk")
        .arg(arg_profile.clone())
        .arg(arg_arch.clone())
        .arg(arg_platform.clone())
        .about("Create disk image from compiled Twizzler components");
    let qemu = SubCommand::with_name("start-qemu")
        .about("Start QEMU instance using the disk image created with twizzler-xtask-make-disk")
        .arg(arg_profile.clone())
        .arg(arg_arch.clone())
        .arg(arg_platform.clone())
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
        .subcommand(build_std_sc)
        .subcommand(check);
    let matches = app.clone().get_matches();
    let (sub_name, sub_matches) = matches.subcommand();

    if sub_matches.is_none() {
        app.print_long_help()?;
        return Err("".into());
    }

    let sub_matches = sub_matches.unwrap();
    let profile = match sub_matches.value_of("profile") {
        Some("debug") => Profile::Debug,
        Some("release") => Profile::Release,
        None => Profile::Debug,
        _ => unreachable!(),
    };

    let arch = match sub_matches.value_of("arch") {
        Some("x86_64") => Arch::X86,
        Some("aarch64") => Arch::Aarch64,
        None => Arch::X86,
        _ => unreachable!(),
    };

    let platform = match sub_matches.value_of("platform") {
        Some("unknown") => Platform::Unknown,
        Some("rpi3") => Platform::Rpi3,
        None => Platform::Unknown,
        _ => unreachable!(),
    };

    let qemu_profile = match sub_matches.value_of("qemu-profile") {
        Some("accel") => QemuProfile::Accel,
        Some("emu") => QemuProfile::Emu,
        None => QemuProfile::Accel,
        _ => unreachable!(),
    };

    let build_info = BuildInfo {
        arch,
        platform,
        profile,
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
        "build-all" => build_all(&meta, &args, build_info)?,
        "check-all" => check_all(&meta, &args, build_info)?,
        "make-disk" => make_disk(&meta, &args, build_info)?,
        "start-qemu" => start_qemu(&meta, &args, build_info, qemu_profile, &qemu_args)?,
        "build-std" => build_std()?,
        _ => unreachable!(),
    }
    Ok(())
}

fn build_crtx(name: &str, build_info: &BuildInfo) -> Result<(), DynError> {
    let objname = format!("{}.o", name);
    let srcname = format!("{}.rs", name);
    let sourcepath = Path::new("toolchain/src/").join(srcname);
    let objpath = format!(
        "toolchain/install/lib/rustlib/{}/lib/{}",
        build_info.get_twizzler_triple(),
        objname
    );
    let objpath = Path::new(&objpath);
    println!("building {:?} => {:?}", sourcepath, objpath);
    let status = Command::new("toolchain/install/bin/rustc")
        .arg("--emit")
        .arg("obj")
        .arg("-o")
        .arg(objpath)
        .arg(sourcepath)
        .arg("--crate-type")
        .arg("staticlib")
        .arg("-C")
        .arg("panic=abort")
        .arg("--target")
        .arg(build_info.get_twizzler_triple())
        .status()?;
    if !status.success() {
        return Err("failed to compile crtx".into());
    }

    Ok(())
}

fn read_stamp() -> Option<String> {
    let mut file = std::fs::File::open("toolchain/install/stamp").ok()?;
    let mut rand = vec![];
    file.read_to_end(&mut rand)
        .expect("failed to read stamp file");
    Some(String::from_utf8(rand).expect("stamp file corrupted"))
}

fn get_twizzler_toolchain_name() -> String {
    let stamp = read_stamp().expect("failed to read stamp file -- did you run cargo bootstrap?");
    format!("twizzler-{}", stamp)
}

fn create_stamp() -> String {
    let rand: String = rand::thread_rng()
        .sample_iter(&rand::distributions::Alphanumeric)
        .take(8)
        .map(char::from)
        .collect();
    let mut file =
        std::fs::File::create("toolchain/install/stamp").expect("failed to create stamp file");
    file.write_all(rand.as_bytes())
        .expect("failed to write stamp file");
    rand
}

enum ReinstallReq {
    No,
    Yes,
    YesAndSubmod,
}

fn needs_reinstall() -> ReinstallReq {
    let stamp = std::fs::metadata("toolchain/install/stamp");
    if stamp.is_err() {
        return ReinstallReq::YesAndSubmod;
    }
    let stamp = stamp
        .unwrap()
        .modified()
        .expect("failed to get system time from metadata");
    for entry in walkdir::WalkDir::new("src/lib/twizzler-abi").min_depth(1) {
        let entry = entry.expect("error walking directory");
        let stat = entry.metadata().expect(&format!(
            "failed to read metadata for {}",
            entry.path().display()
        ));
        let mtime = stat
            .modified()
            .expect("failed to get system time from mtime");

        if mtime >= stamp {
            return ReinstallReq::Yes;
        }
    }
    ReinstallReq::No
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
            return Err("failed to update git submodules".into());
        }
    }

    let res = std::fs::hard_link(
        "toolchain/src/config.toml",
        "toolchain/src/rust/config.toml",
    );
    if let Err(e) = res {
        if e.kind() != std::io::ErrorKind::AlreadyExists {
            return Err("failed to create hardlink config.toml".into());
        }
    }
    let res = std::fs::remove_dir_all("toolchain/src/rust/library/twizzler-abi");
    if let Err(e) = res {
        if e.kind() != std::io::ErrorKind::NotFound {
            return Err("failed to remove softlink twizzler-abi".into());
        }
    }
    fs_extra::copy_items(
        &["src/lib/twizzler-abi"],
        "toolchain/src/rust/library/",
        &CopyOptions::new(),
    )?;

    let status = Command::new("./x.py")
        .arg("install")
        .current_dir("toolchain/src/rust")
        .status()?;
    if !status.success() {
        return Err("failed to compile rust toolchain".into());
    }

    for bi in &all_supported_build_infos() {
        build_crtx("crti", bi)?;
        build_crtx("crtn", bi)?;
    }

    let _stamp = create_stamp();
    eprintln!(
        "Adding toolchain {} => toolchain/install",
        get_twizzler_toolchain_name()
    );
    /* add to toolchain */
    let status = Command::new("rustup")
        .arg("toolchain")
        .arg("link")
        .arg(&get_twizzler_toolchain_name())
        .arg("toolchain/install")
        .status()?;
    if !status.success() {
        return Err("failed to link rust Twizzler toolchain with rustup".into());
    }

    Ok(())
}

fn build_std() -> Result<(), DynError> {
    let res = std::fs::remove_dir_all("toolchain/src/rust/library/twizzler-abi");
    if let Err(e) = res {
        if e.kind() != std::io::ErrorKind::NotFound {
            return Err("failed to remove softlink twizzler-abi".into());
        }
    }
    fs_extra::copy_items(
        &["src/lib/twizzler-abi"],
        "toolchain/src/rust/library/",
        &CopyOptions::new(),
    )?;

    let status = Command::new("./x.py")
        .arg("build")
        .arg("--keep-stage")
        .arg("0")
        .arg("--keep-stage")
        .arg("1")
        .arg("--keep-stage-std")
        .arg("0")
        .arg("--keep-stage-std")
        .arg("1")
        .arg("--target")
        .arg("x86_64-unknown-twizzler") //TODO
        .arg("--incremental")
        .arg("--stage")
        .arg("2")
        .arg("library/std")
        .current_dir("toolchain/src/rust")
        .status()?;
    if !status.success() {
        return Err("failed to compile rust toolchain".into());
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn cargo_cmd_collection(
    meta: &Metadata,
    collection_name: &str,
    cargo_cmd: &str,
    wd: &str,
    args: &[String],
    rustflags: Option<String>,
    build_info: BuildInfo,
    triple: Option<String>,
    use_toolchain: bool,
) -> Result<(), DynError> {
    eprintln!(
        "== BUILDING COLLECTION {} ({}) [{}] ==",
        collection_name,
        build_info,
        if use_toolchain {
            get_twizzler_toolchain_name()
        } else {
            String::from("nightly")
        }
    );
    if use_toolchain {
        let res = needs_reinstall();
        match res {
            ReinstallReq::Yes => {
                eprintln!("ERROR: detected that files in twizzler-abi crate are newer than the installed toolchain. Please run `cargo bootstrap --skip-submodules' to update the toolchain.");
                Err("toolchain not updated")?;
            }
            ReinstallReq::YesAndSubmod => {
                eprintln!(
                    "ERROR: did not detect installed toolchain Did you run `cargo bootstrap`?"
                );
                Err("toolchain not found")?;
            }
            ReinstallReq::No => {}
        }
    }

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
    let mut target_args = vec![];
    if let Some(ref triple) = triple {
        target_args.push("--target".to_owned());
        target_args.push(triple.to_owned());
    }
    let mut status = Command::new(cargo);
    status
        .current_dir(wd)
        .arg(cargo_cmd)
        .args(pkg_list)
        .args(target_args)
        .args(args);
    if let Some(s) = rustflags {
        status.env("RUSTFLAGS", s);
    }
    if use_toolchain {
        status.env("RUSTUP_TOOLCHAIN", get_twizzler_toolchain_name());
    }

    let status = status.status()?;
    if !status.success() {
        return Err("failed to run cargo command".into());
    }
    Ok(())
}

fn cmd_all(
    meta: &Metadata,
    args: &[String],
    cargo_cmd: &str,
    build_info: BuildInfo,
) -> Result<(), DynError> {
    cargo_cmd_collection(
        meta, "tools", cargo_cmd, ".", args, None, build_info, None, false,
    )?;
    cargo_cmd_collection(
        meta,
        "kernel",
        cargo_cmd,
        "src/kernel",
        args,
        None,
        build_info,
        None,
        false,
    )?;
    if false {
        let mut co = CopyOptions::new();
        co.overwrite = true;
        fs_extra::copy_items(
            &["src/lib/twizzler-abi"],
            "toolchain/install/lib/rustlib/src/rust/library/",
            &co,
        )?;
    }
    let args = args.to_vec();
    //args.push("-vv".to_owned());
    cargo_cmd_collection(
        meta,
        "twizzler-apps",
        cargo_cmd,
        ".",
        &args,
        None,
        build_info,
        Some(build_info.get_twizzler_triple()),
        true,
    )?;
    Ok(())
}

fn check_all(meta: &Metadata, args: &[String], build_info: BuildInfo) -> Result<(), DynError> {
    let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    for p in meta.workspace_metadata["checks"].as_array().unwrap().iter() {
        let mut status = Command::new(&cargo);
        let status = status
            .current_dir(format!("src/lib/{}", p.as_str().unwrap()))
            .arg("check")
            .arg("-p")
            .arg(p.as_str().unwrap())
            .args(args)
            .status()?;
        if !status.success() {
            // Err("failed to run cargo command")?;
        }
    }
    cmd_all(meta, args, "check", build_info)?;
    Ok(())
}

fn build_all(meta: &Metadata, args: &[String], build_info: BuildInfo) -> Result<(), DynError> {
    cmd_all(meta, args, "build", build_info)?;
    Ok(())
}

fn make_path(build_info: BuildInfo, kernel: bool, name: &str) -> String {
    format!(
        "target/{}/{}/{}",
        if kernel {
            build_info.get_kernel_triple()
        } else {
            build_info.get_twizzler_triple()
        },
        build_info.profile.as_str(),
        name
    )
}

fn make_tool_path(build_info: BuildInfo, name: &str) -> String {
    format!("target/{}/{}", build_info.profile.as_str(), name)
}

fn make_disk(meta: &Metadata, args: &[String], build_info: BuildInfo) -> Result<(), DynError> {
    build_all(meta, args, build_info)?;
    let pkg_list: Vec<String> = meta.workspace_metadata["initrd-members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.to_string().replace("\"", ""))
        .collect();
    let initrd_files: Vec<String> = pkg_list
        .iter()
        .map(|x| make_path(build_info, false, x))
        .collect();
    eprintln!("== BUILDING INITRD ({}) ==", build_info);
    let status = Command::new(make_tool_path(build_info, "initrd_gen"))
        .arg("--output")
        .arg(make_path(build_info, true, "initrd"))
        .args(&initrd_files)
        .status()?;
    if !status.success() {
        return Err("failed to generate initrd".into());
    }
    eprintln!("== BUILDING DISK IMAGE ({}) ==", build_info);
    let status = Command::new(make_tool_path(build_info, "image_builder"))
        .arg(make_path(build_info, true, "twizzler-kernel"))
        .arg(make_path(build_info, true, "initrd"))
        .status()?;

    if !status.success() {
        return Err("disk image creation failed".into());
    }
    Ok(())
}

fn start_qemu(
    meta: &Metadata,
    args: &[String],
    build_info: BuildInfo,
    qemu_profile: QemuProfile,
    qemu_args: &[String],
) -> Result<(), DynError> {
    make_disk(meta, args, build_info)?;
    let mut run_cmd = Command::new("qemu-system-x86_64");
    run_cmd.arg("-m").arg("1024,slots=4,maxmem=8G");
    run_cmd.arg("-bios").arg("/usr/share/qemu/OVMF.fd");
    run_cmd.arg("-smp").arg("4,sockets=1,cores=2,threads=2");
    run_cmd.arg("-drive").arg(format!(
        "format=raw,file={}",
        make_path(build_info, true, "disk.img")
    ));
    run_cmd.arg("-machine").arg("q35,nvdimm=on");
    run_cmd.arg("-object").arg(format!(
        "memory-backend-file,id=mem1,share=on,mem-path={},size=4G",
        make_path(build_info, true, "pmem.img")
    ));
    //run_cmd.arg("-d").arg("trace:*ioapic*");
    run_cmd.arg("-device").arg("nvdimm,id=nvdimm1,memdev=mem1");
    const RUN_ARGS: &[&str] = &["--no-reboot", "-s", "-serial", "mon:stdio"]; //, "-vnc", ":0"];
    run_cmd.args(RUN_ARGS);
    run_cmd.args(qemu_profile.get_args());
    run_cmd.args(qemu_args);

    let exit_status = run_cmd.status().unwrap();
    if !exit_status.success() {
        return Err("failed to run qemu".into());
    }
    Ok(())
}
