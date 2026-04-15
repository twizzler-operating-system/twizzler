use crate::triple::{all_possible_platforms, Triple};

mod python3;

pub fn build_and_install_ports() -> anyhow::Result<()> {
    for target in all_possible_platforms() {
        build_ports(&target)?;
    }

    Ok(())
}

fn build_ports(triple: &Triple) -> anyhow::Result<()> {
    println!("Building python3 for {}", triple);
    python3::install(triple)?;

    Ok(())
}
