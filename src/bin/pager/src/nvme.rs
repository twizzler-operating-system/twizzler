use twizzler_driver::device::Device;

struct NvmeHeader {}

struct NvmeDevice {
    dev: Device,
    mmio_base: *const NvmeHeader,
}

impl NvmeDevice {
    fn new(dev: Device) -> Self {
        let header = unsafe { dev.get_mmio(0).unwrap().get_mmio_offset(0) as *const NvmeHeader };
        Self {
            dev,
            mmio_base: header,
        }
    }

    fn get_header(&self) -> &NvmeHeader {
        unsafe { self.mmio_base.as_ref().unwrap_unchecked() }
    }
}
