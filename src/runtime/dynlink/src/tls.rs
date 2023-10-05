use crate::compartment::CompartmentAlloc;

#[derive(Default)]
pub(crate) struct TlsInfo {
    gen_count: u64,
    tls_mods: Vec<TlsModule>,
}

pub(crate) struct TlsModule {
    is_static: bool,
    template_addr: usize,
    template_filesz: usize,
    template_memsz: usize,
    template_align: usize,
}

impl TlsModule {
    pub(crate) fn new_static(
        template_addr: usize,
        template_filesz: usize,
        template_memsz: usize,
        template_align: usize,
    ) -> Self {
        Self {
            is_static: true,
            template_addr,
            template_filesz,
            template_memsz,
            template_align,
        }
    }
}

impl TlsInfo {
    pub fn insert(&mut self, tm: TlsModule) -> TlsModId {
        let id = self.tls_mods.len();
        self.tls_mods.push(tm);
        self.gen_count += 1;
        TlsModId((id + 2) as u64)
    }
}

#[repr(transparent)]
pub(crate) struct TlsModId(u64);

impl TlsModId {
    pub(crate) fn as_index(&self) -> usize {
        assert!(self.0 >= 2);
        (self.0 - 2) as usize
    }

    pub(crate) fn as_tls_id(&self) -> u64 {
        self.0
    }
}
