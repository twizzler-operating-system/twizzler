#![feature(naked_functions)]
#![feature(twizzler)]

use std::sync::Arc;

use dynlink::{symbol::LookupFlags, DynlinkError};
use state::MonitorState;
use tracing::{debug, info, trace, warn, Level};
use tracing_subscriber::{fmt::format::FmtSpan, FmtSubscriber};
use twizzler_abi::{
    aux::KernelInitInfo,
    object::{MAX_SIZE, NULLPAGE_SIZE},
    syscall::{
        sys_object_create, BackingType, LifetimeType, ObjectCreate, ObjectCreateFlags, ObjectSource,
    },
};
use twizzler_object::{slot::Slot, ObjID, Object, ObjectInitFlags, Protections};
use twizzler_runtime_api::ObjectHandle;

use crate::runtime::init_actions;

mod init;
mod runtime;
mod state;

pub mod secgate_test;

pub fn main() {
    std::env::set_var("RUST_BACKTRACE", "full");
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::TRACE)
        .with_target(false)
        .with_span_events(FmtSpan::ACTIVE)
        .finish();

    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    trace!("monitor entered, discovering dynlink context");
    let init =
        init::bootstrap_dynlink_context().expect("failed to discover initial dynlink context");
    let state = Arc::new(state::MonitorState::new(init));
    debug!("found dynlink context, with root {}", state.root);

    init_actions(state.clone());
    std::env::set_var("RUST_BACKTRACE", "1");

    let main_thread = std::thread::spawn(|| monitor_init(state));
    main_thread.join().unwrap();
    warn!("monitor main thread exited");
}

fn monitor_init(state: Arc<MonitorState>) {
    info!("monitor early init completed, starting init");

    /*
    let hw = Object::init_id(
        find_init_name("libhello_world.so").unwrap(),
        Protections::READ,
        ObjectInitFlags::empty(),
    )
    .unwrap();
    let lib = dynlink::library::Library::new(hw, "libhello_world.so");
    let comp = state.dynlink.add_compartment("test").unwrap();

    let hwlib = state
        .dynlink
        .add_library(&comp, lib, &mut Loader {})
        .unwrap();

    state.dynlink.relocate_all([hwlib.clone()]).unwrap();

    info!("lookup entry");
    let sym = state
        .dynlink
        .lookup_symbol(&hwlib, "test_sec_call", LookupFlags::empty())
        .unwrap();

    let addr = sym.reloc_value();
    info!("addr = {:x}", addr);
    let ptr: extern "C" fn() = unsafe { core::mem::transmute(addr as usize) };
    (ptr)();
    */
}

struct Loader {}
pub fn get_kernel_init_info() -> &'static KernelInitInfo {
    unsafe {
        (((twizzler_abi::slot::RESERVED_KERNEL_INIT * MAX_SIZE) + NULLPAGE_SIZE)
            as *const KernelInitInfo)
            .as_ref()
            .unwrap()
    }
}

fn find_init_name(name: &str) -> Option<ObjID> {
    let init_info = get_kernel_init_info();
    for n in init_info.names() {
        if n.name() == name {
            return Some(n.id());
        }
    }
    None
}

/*
impl LibraryLoader for Loader {
    fn create_segments(
        &mut self,
        data_cmds: &[ObjectSource],
        text_cmds: &[ObjectSource],
    ) -> Result<(Object<u8>, Object<u8>), dynlink::DynlinkError> {
        let create_spec = ObjectCreate::new(
            BackingType::Normal,
            LifetimeType::Volatile,
            None,
            ObjectCreateFlags::empty(),
        );
        let data_id =
            sys_object_create(create_spec, &data_cmds, &[]).map_err(|_| DynlinkError::Unknown)?;
        let text_id =
            sys_object_create(create_spec, &text_cmds, &[]).map_err(|_| DynlinkError::Unknown)?;

        // TODO
        let data = Object::init_id(
            data_id,
            Protections::READ | Protections::WRITE,
            ObjectInitFlags::empty(),
        )
        .map_err(|_| DynlinkError::Unknown)?;
        let text = Object::init_id(
            text_id,
            Protections::READ | Protections::EXEC,
            ObjectInitFlags::empty(),
        )
        .map_err(|_| DynlinkError::Unknown)?;

        Ok((data, text))
    }

    fn open(&mut self, mut name: &str) -> Result<Object<u8>, dynlink::DynlinkError> {
        if name.starts_with("libstd") {
            name = "libstd.so"
        }
        let id = find_init_name(name).unwrap();
        let obj = Object::init_id(id, Protections::READ, ObjectInitFlags::empty())
            .map_err(|_| DynlinkError::Unknown)?;
        Ok(obj)
    }
}
*/
