use crate::KaniOptions;
use std::process::Command;
use std::io::ErrorKind;

use anyhow::bail;


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


fn pre_fixed_flags() -> Vec<String> {
    let mut flags: Vec<_> = Vec::new();

    flags.extend_from_slice(
        &[
            "--enable-unstable",
            "-Zunstable-options",
            "--ignore-global-asm",
            "--output-into-files",
        ]
        .map(String::from)
        .to_vec(),
    );

    flags
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

#[derive(Clone, Debug)]
struct PipelineSpec<T> {
    base_args: Vec<T>,
    extra_args: Vec<String>,
    output_file: Option<T>,
    path: String,
    env: Option<T>,
}

pub(crate) fn launch_kani_pipelined(cli: KaniOptions) -> anyhow::Result<()>{

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

    let bin_path = format!(
        "{}:{}",
        std::fs::canonicalize("toolchain/install/bin")
            .unwrap()
            .to_string_lossy(),
        path
    );

    // Set local path
    // std::env::set_var(
    //     "PATH",
    //     bin_path,
    // );

    //Base command
    let mut base_string = vec!["kani".to_string()];

    //Add Command Line Kani Args
    if let Some(args) = cli.kani_options {
        base_string.push(args);
    }

    //Add Command Line CBMC Args
    if let Some(args) = cli.cbmc_options {
        base_string.push("--cbmc-args".to_string());
        base_string.push(args);
    }

    base_string.append(&mut pre_fixed_flags());

    let  supported_packeges = packages_with_harnesses();

    let mut command_vec = vec![];

    if let Some(args) = cli.select_supported_package {
        // Check if we support requested package
        if supported_packeges.iter().any(|e| args.contains(&args)){

            let mut package_arg = vec!["-p".to_string()];
            package_arg.push(args);

            command_vec.push(
                PipelineSpec {
                    base_args: base_string,
                    extra_args: package_arg,
                    output_file: None,
                    path: bin_path,
                    env: None
                }
            );

        }else {
            anyhow::bail!("Package {} not suppported", args)
        }
    }
    else {

        for package in supported_packeges.iter() {
            command_vec.push(
                PipelineSpec {
                    base_args: base_string.clone(),
                    extra_args: vec!["-p".to_string(), package.to_string()] ,
                    output_file: None,
                    path: bin_path.clone(),
                    env: None
                }
            );
        }
    }

    for spec in command_vec {
        println!("---------");
        println!("{:?}", spec);

        //TODO: Spawn as children, run in parallel instead of serially.
        //TODO: No need to handle verification ouput, kani handles that but relevant to try to handle build output
        let  mut cmd = Command::new("cargo")
        .args(&spec.base_args)
        .args(&spec.extra_args)
        .env("PATH", &spec.path)
        // .stdout(Stdio::piped())
        .spawn()
        .expect("could not spawn");

        cmd.wait_with_output().expect("Error running command");
    }

    Ok(())

}