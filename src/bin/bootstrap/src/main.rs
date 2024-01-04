use std::process::exit;

use dynlink::{
    context::engine::ContextEngine,
    engines::Engine,
    library::{BackingData, Library, UnloadedLibrary},
    symbol::LookupFlags,
    DynlinkError,
};
use tracing::{debug, info, trace, warn, Level};
use tracing_subscriber::FmtSubscriber;
use twizzler_abi::{
    arch::SLOTS,
    object::{ObjID, Protections},
    syscall::{
        sys_object_create, sys_object_read_map, BackingType, LifetimeType, ObjectCreate,
        ObjectCreateFlags, ObjectSource,
    },
};
use twizzler_object::{Object, ObjectInitFlags};
use twizzler_runtime_api::AuxEntry;

fn find_init_name(name: &str) -> Option<ObjID> {
    let init_info = twizzler_abi::runtime::get_kernel_init_info();
    for n in init_info.names() {
        if n.name() == name {
            return Some(n.id());
        }
    }
    None
}

fn start_runtime(runtime_monitor: ObjID, _runtime_library: ObjID) -> ! {
    let engine = Engine;
    let mut ctx = dynlink::context::Context::new(engine);
    let unlib = UnloadedLibrary::new("libmonitor.so");
    let mut monitor_compartment = ctx.add_compartment("monitor").unwrap();

    let monitor = ctx
        .load_library_in_compartment(todo!(), unlib, |name| todo!())
        .unwrap();

    ctx.relocate_all(monitor).unwrap();
    let tls = monitor_compartment.build_tls_region(()).unwrap();

    let monitor = monitor_compartment.root_library();
    debug!("context loaded, jumping to monitor");
    let entry = ctx
        .lookup_symbol(
            &monitor,
            "monitor_entry_from_bootstrap",
            LookupFlags::empty(),
        )
        .unwrap();

    let value = entry.reloc_value() as usize;
    let ptr: extern "C" fn(usize) = unsafe { core::mem::transmute(value) };

    let mut info = ctx.build_runtime_info(monitor, tls).unwrap();
    let info_ptr = &info as *const _ as usize;
    let aux = vec![AuxEntry::RuntimeInfo(info_ptr), AuxEntry::Null];

    let mut used = vec![];
    used.reserve(SLOTS);
    // No more memory allocation after this point. We scan the address space to build a list
    // of used slots for the next runtime.
    for slot in 0..SLOTS {
        let r = sys_object_read_map(None, slot);
        if r.is_ok() {
            used.push(slot);
        }
    }
    info.used_slots = used;

    let aux_ptr = aux.as_slice().as_ptr();
    trace!("jumping to {:x}", value);
    (ptr)(aux_ptr as usize);

    warn!("returned from monitor, exiting...");
    exit(0);
}

/*
struct Loader {}

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

        let text = Object::init_id(
            text_id,
            Protections::READ | Protections::EXEC,
            ObjectInitFlags::empty(),
        )
        .map_err(|_| DynlinkError::Unknown)?;

        let data = Object::init_id(
            data_id,
            Protections::READ | Protections::WRITE,
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

fn main() {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::DEBUG)
        .finish();

    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    let runtime_lib = find_init_name("libtwz_rt.so").unwrap();
    let monitor = find_init_name("libmonitor.so").unwrap();

    info!("bootstrapping runtime monitor");
    start_runtime(monitor, runtime_lib);
}
