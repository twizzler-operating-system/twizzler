

use crate::KaniOptions;
use std::process::Command;
use std::env;


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


    //Actually run the command
    let mut cmd = Command::new("cargo");
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
            "--ignore-global-asm"
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
        ].map(String::from)
        .to_vec());

    exclude_packages
}