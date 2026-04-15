use crate::triple::{Arch, Host, Machine, Triple};

mod llvm;
mod ncurses;
mod python3;
mod zlib;
//mod rust;

#[derive(clap::Parser, Debug)]
pub struct PortOptions {
    #[clap(long, help = "The target architecture.", default_value = "x86-64")]
    pub arch: Arch,
    pub ports: Vec<String>,
}

pub fn build_and_install_ports(cli: &PortOptions) -> anyhow::Result<()> {
    let triple = Triple::new(cli.arch, Machine::Unknown, Host::Twizzler, None);
    if cli.ports.is_empty() {
        build_ports(&triple)?;
        return Ok(());
    }

    for port in &cli.ports {
        match port.as_str() {
            "python3" => python3::install(&triple)?,
            "llvm" => llvm::install(&triple)?,
            "zlib" => zlib::install(&triple)?,
            "ncurses" => ncurses::install(&triple)?,
            //"rust" => rust::install(&triple)?,
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

    Ok(())
}
