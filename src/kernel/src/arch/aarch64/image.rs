/// TLS initialization for the kernel image
use crate::{
    image::{TlsInfo, TlsVariant},
    memory::VirtAddr,
};

pub fn init_tls(tls_template: TlsInfo) -> VirtAddr {
    crate::image::init_tls(TlsVariant::Variant1, tls_template)
}
