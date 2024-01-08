use std::sync::{Arc, Mutex, OnceLock};

use dynlink::library::BackingData;
use twizzler_abi::object::{MAX_SIZE, NULLPAGE_SIZE};
use twizzler_runtime_api::{AddrRange, DlPhdrInfo, Library, LibraryId};
use twz_rt::monitor::MonitorActions;

use crate::state::MonitorState;

#[no_mangle]
pub fn __do_get_monitor_actions() -> &'static dyn MonitorActions {
    ACTIONS.get().unwrap()
}

struct MonitorActionsImpl {
    state: Arc<Mutex<MonitorState>>,
}

pub(crate) fn init_actions(state: Arc<Mutex<MonitorState>>) {
    let _ = ACTIONS.set(MonitorActionsImpl { state });
}

impl MonitorActions for MonitorActionsImpl {
    fn lookup_library_by_id(
        &self,
        id: twizzler_runtime_api::LibraryId,
    ) -> Option<twizzler_runtime_api::Library> {
        let state = self.state.lock().unwrap();
        let lib = state.get_nth_library(id.0)?;
        let next_id = LibraryId(id.0 + 1);
        let phdrs = lib.get_phdrs_raw()?;

        Some(Library {
            mapping: lib.full_obj.clone().to_inner(),
            range: (lib.full_obj.data().0, unsafe {
                lib.full_obj.data().0.add(lib.full_obj.data().1)
            }),
            dl_info: Some(DlPhdrInfo {
                addr: lib.base_addr(),
                name: core::ptr::null(),
                phdr_start: phdrs.0 as *const _,
                phdr_num: phdrs.1.try_into().ok()?,
                _adds: 0,
                _subs: 0,
                modid: lib
                    .tls_id
                    .map(|t| t.tls_id())
                    .unwrap_or(0)
                    .try_into()
                    .ok()?,
                tls_data: core::ptr::null(),
            }),
            next_id: state.get_nth_library(next_id.0).map(|_| next_id),
            id,
        })
    }

    fn local_primary(&self) -> Option<twizzler_runtime_api::LibraryId> {
        Some(LibraryId(0))
    }

    fn lookup_library_name(&self, id: LibraryId, buf: &mut [u8]) -> Option<usize> {
        let state = self.state.lock().unwrap();
        let lib = state.get_nth_library(id.0)?;
        if buf.len() < lib.name.len() {
            return None;
        }
        buf[0..lib.name.len()].copy_from_slice(&lib.name.as_bytes());
        Some(lib.name.len())
    }

    fn get_segment(&self, id: LibraryId, seg: usize) -> Option<twizzler_runtime_api::AddrRange> {
        const PT_LOAD: u32 = 1;
        let state = self.state.lock().unwrap();
        let lib = state.get_nth_library(id.0)?;
        let phdrs = lib.get_phdrs_raw()?;
        let slice = unsafe { core::slice::from_raw_parts(phdrs.0, phdrs.1) };
        let phdr = slice.iter().filter(|p| p.p_type == PT_LOAD).nth(seg)?;
        Some(AddrRange {
            start: lib.laddr::<u8>(phdr.p_vaddr) as usize,
            len: phdr.p_memsz as usize,
        })
    }

    fn allocate_tls_region(&self) -> Option<dynlink::tls::TlsRegion> {
        let tcb = twz_rt::monitor::RuntimeThreadControl::new();

        let comp = self
            .state
            .lock()
            .unwrap()
            .dynlink
            .lookup_compartment("monitor")
            .unwrap();

        self.state
            .lock()
            .unwrap()
            .dynlink
            .get_compartment_mut(comp)
            .build_tls_region(tcb)
            .ok()
    }

    fn free_tls_region(&self, tls: dynlink::tls::TlsRegion) {
        drop(tls);
    }
}

static ACTIONS: OnceLock<MonitorActionsImpl> = OnceLock::new();
