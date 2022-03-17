use crate::BootstrapOptions;

pub fn get_toolchain_path() -> anyhow::Result<String> {
    Ok("toolchain/install".to_string())
}

pub fn get_rustc_path() -> anyhow::Result<String> {
    let toolchain = get_toolchain_path()?;
    Ok(format!("{}/bin/rustc", toolchain))
}

pub(crate) fn do_bootstrap(_cli: BootstrapOptions) -> anyhow::Result<()> {
    todo!()
}

pub(crate) fn init_for_build() -> anyhow::Result<()> {
    std::env::set_var("RUSTC", &get_rustc_path()?);
    Ok(())
}
