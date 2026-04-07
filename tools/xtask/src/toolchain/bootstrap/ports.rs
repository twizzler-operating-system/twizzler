use crate::{
    toolchain::BootstrapOptions,
    triple::{all_possible_platforms, Triple},
};

mod python3;

pub fn build_and_install_ports(cli: &BootstrapOptions) -> anyhow::Result<()> {
    for target in all_possible_platforms() {
        build_ports(cli, &target)?;
    }

    Ok(())
}

fn build_ports(cli: &BootstrapOptions, triple: &Triple) -> anyhow::Result<()> {
    println!("Building python3 for {}", triple);
    python3::install(cli, triple)?;

    Ok(())
}
