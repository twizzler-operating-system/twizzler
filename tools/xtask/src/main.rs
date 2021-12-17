use std::{env, process::Command, str::FromStr};

use cargo_metadata::{Metadata, MetadataCommand};

type DynError = Box<dyn std::error::Error>;

fn main() {
    if let Err(e) = try_main() {
        eprintln!("{}", e);
        std::process::exit(101);
    }
}

#[derive(Copy, Clone, Debug)]
enum Profile {
    Debug,
    Release,
}
fn try_main() -> Result<(), DynError> {
    let path = "Cargo.toml";
    let meta = MetadataCommand::new().manifest_path(path).exec().unwrap();
    let task = env::args().nth(1);
    let args = &env::args().into_iter().collect::<Vec<String>>()[2..];
    let profile = if args.contains(&"--release".to_owned()) {
        Profile::Release
    } else {
        Profile::Debug
    };
    match task.as_ref().map(|it| it.as_str()) {
        Some("build-all") => build_all(&meta, args, profile)?,
        Some("check-all") => check_all(&meta, args, profile)?,
        Some("make-disk") => make_disk(&meta, args, profile)?,
        _ => print_help(),
    }
    Ok(())
}

fn print_help() {
    println!("xtask help TODO")
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
