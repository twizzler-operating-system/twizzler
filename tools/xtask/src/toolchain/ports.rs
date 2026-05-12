use crate::triple::{Arch, Host, Machine, Triple};

mod binutils;
mod curl;
mod llvm;
mod ncurses;
mod openssl;
mod psl;
mod python3;
mod rust;
mod zlib;

#[derive(clap::Parser, Debug)]
pub struct PortOptions {
    #[clap(long, help = "The target architecture.", default_value = "x86-64")]
    pub arch: Arch,
    pub ports: Vec<String>,
}

pub fn list_ports() -> anyhow::Result<()> {
    let ports = vec![
        ("python3", "zlib,openssl,ncurses"),
        ("llvm", "zlib"),
        ("zlib", ""),
        ("ncurses", ""),
        //("rust", ""),
        ("openssl", "zlib"),
        ("curl", "zlib,openssl"),
        ("psl", ""),
        ("binutils", ""),
    ];

    for port in ports {
        if port.1.is_empty() {
            println!("{}", port.0);
        } else {
            println!("{} (requires {})", port.0, port.1);
        }
    }

    println!("\nTo compile all ports, run cargo toolchain ports @all");

    Ok(())
}

pub fn build_and_install_ports(cli: &PortOptions) -> anyhow::Result<()> {
    let triple = Triple::new(cli.arch, Machine::Unknown, Host::Twizzler, None);
    if cli.ports.is_empty() {
        return list_ports();
    }

    for port in &cli.ports {
        if port == "@all" {
            build_ports(&triple)?;
            continue;
        }
        match port.as_str() {
            "python3" => python3::install(&triple)?,
            "llvm" => llvm::install(&triple)?,
            "zlib" => zlib::install(&triple)?,
            "ncurses" => ncurses::install(&triple)?,
            // in-progress support
            "rust" => rust::install(&triple)?,
            "openssl" => openssl::install(&triple)?,
            "curl" => curl::install(&triple)?,
            "psl" => psl::install(&triple)?,
            "binutils" => binutils::install(&triple)?,
            _ => anyhow::bail!("Unknown port: {}", port),
        }
    }

    Ok(())
}

fn build_ports(triple: &Triple) -> anyhow::Result<()> {
    python3::install(triple)?;
    zlib::install(triple)?;
    ncurses::install(triple)?;
    llvm::install(triple)?;
    //rust::install(triple)?;
    openssl::install(triple)?;
    psl::install(triple)?;
    curl::install(triple)?;
    binutils::install(triple)?;

    Ok(())
}
