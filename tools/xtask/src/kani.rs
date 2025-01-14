use crate::KaniOptions;
use std::process::Command;
use std::io::ErrorKind;

use anyhow::bail;

//Verifies Kani is installed and launches it
pub(crate) fn launch_kani(cli: KaniOptions) -> anyhow::Result<()> {
    //Check Kani is installed
    match Command::new("cargo").args(["kani","--version"]).spawn() {
        Ok(_) => println!("Kani installed!"),
        Err(e) => {
            if e.kind() == ErrorKind::NotFound {
                println!("error: {}",e.kind());
                bail!("Kani not installed!")
            } else {
                println!("error: {}",e.kind());
                println!("Unknown error")
            }
        }
    };



    // Handle path
    let path = std::env::var("PATH").unwrap();
    std::env::set_var(
        "PATH",
        format!(
            "{}:{}",
            std::fs::canonicalize("toolchain/install/bin")
                .unwrap()
                .to_string_lossy(),
            path
        ),
    );

    //Compose the command
    let mut cmd = Command::new("cargo");
    cmd.arg("kani");
    cmd.args(kernel_flags());
    
    //Add Kani Args
    if let Some(args) = cli.kani_options {
        cmd.arg(args);
    }

    //Capture CBMC options
    if let Some(args) = cli.cbmc_options {
        cmd.args(cbmc_flags());
        cmd.arg(args);
    }

    let  supported_packeges = packages_with_harnesses();

    cmd.arg("-p");
    cmd.args(supported_packeges);



    let status = cmd.status()?;
         if !status.success() {
            println!("Kani Command Faillure: {}", pretty_cmd(&cmd));
        }

    Ok(())

}

fn pretty_cmd(cmd: &Command) -> String {
    format!(
        "{} {:?}",
        cmd.get_envs()
            .map(|(key, val)| format!("{:?}={:?}", key, val))
            .fold(String::new(), |a, b| a + &b),
        cmd
    )
}


fn packages_with_harnesses() -> Vec<String> {
    let mut packages: Vec<_> = Vec::new();

    packages.extend_from_slice(
        &[
            // "twizzler-kernel", TODO: Current syntax erros, toolchain mismap?
            "twizzler-abi",
            "twizzler-driver",
            "twizzler-queue-raw"
        ]
        .map(String::from)
        .to_vec(),
    );

    packages
} 


fn kernel_flags() -> Vec<String> {
    let mut flags: Vec<_> = Vec::new();

    flags.extend_from_slice(
        &[
            "--enable-unstable",
            "-Zunstable-options",
            "--ignore-global-asm",
            "--output-into-files",
            "-Zstubbing",
        ]
        .map(String::from)
        .to_vec(),
    );

    flags
}

fn cbmc_flags() -> Vec<String> {
    let mut flags: Vec<_> = Vec::new();

    flags.extend_from_slice(
        &[
            "--cbmc-args",
            // "--show-properties"
        ]
        .map(String::from)
        .to_vec(),
    );

    flags
}

fn exclude_list() -> Vec<String> {
    let mut exclude_packages: Vec<_> = Vec::new();

    exclude_packages.extend_from_slice(
        &[
            "--workspace",
            "--exclude",
            "monitor",
        ]
        .map(String::from)
        .to_vec(),
    );

    exclude_packages
}