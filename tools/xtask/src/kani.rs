

use crate::KaniOptions;
use std::fs::{self, File};
use std::path::Path;
use std::process::Command;
use std::env;

use chrono::prelude::*;


//Verifies Kani is installed and launches it
pub(crate) fn launch_kani(cli:  KaniOptions) -> anyhow::Result<()> {


    //Check Kani is installed
    // match Command::new("cargo kani --version").spawn() {
    //     Ok(_) => println!("Kani installed!"),
    //     Err(e) => {
    //         if ErrorKind == e.kind(){
    //             bail!("Kani not installed!")
    //         } else {
    //             println!("Unknown error")
    //         }
    //     }
    // };

    let date = Local::now().format("%Y-%m-%d-%H:%M:%S").to_string();

    if !Path::new("./kani_test/log/").exists(){
        fs::create_dir_all("./kani_test/log/")?;
    }

    let log_name = format!("./kani_test/log/{}.log", date);
    let log = File::create(log_name).expect("failed to open log");


    //Actually run the command
    let mut cmd = Command::new("cargo");
    cmd.stdout(log);
    cmd.arg("kani");
    //Add env 
    // let hash = cli.env.and_then(|vec| Some(vec.into_iter()));
    //Pass any desired environment variables
    cmd.envs(env::vars());


    //Add kani args
    if let Some(args) = cli.kani_options {
        cmd.arg(args);
    }

    cmd.args(exclude_list());
    cmd.args(kernel_flags());

    // let child = cmd.stdout(Stdio::inherit())
    // .stderr(Stdio::inherit()).spawn();

    // return child.
    println!("{:?}", cmd);
    match cmd.spawn() {
        Err(e) => {
            return Err(e.into());
        }
        Ok(_v) => {
            return Ok(());
        }

    }
}

pub fn kernel_flags() -> Vec<String> {
    let mut flags: Vec<_> = Vec::new();

    flags.extend_from_slice(
        &[
            "--enable-unstable",
            "--ignore-global-asm",
            "-Zstubbing"
        ].map(String::from)
        .to_vec());

    flags
}


pub fn exclude_list() -> Vec<String> {

    let mut exclude_packages: Vec<_>= Vec::new();

    exclude_packages.extend_from_slice(
        &[
            "--workspace",
            "--exclude",
            "monitor",
            "unicode-bidi"
            // "twizzler-abi"
        ].map(String::from)
        .to_vec());

    exclude_packages
}
