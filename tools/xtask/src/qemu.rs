use crate::QemuOptions;

pub(crate) fn do_start_qemu(cli: QemuOptions) -> anyhow::Result<()> {
    let image_info = crate::image::do_make_image(cli.into())?;
    Ok(())
}
