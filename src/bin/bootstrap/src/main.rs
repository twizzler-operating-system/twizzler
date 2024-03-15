use std::process::exit;

use dynlink::{
    engines::{Backing, Engine},
    library::UnloadedLibrary,
    symbol::LookupFlags,
};
use tracing::{debug, info, warn, Level};
use tracing_subscriber::FmtSubscriber;
use twizzler_abi::{arch::SLOTS, object::ObjID, syscall::sys_object_read_map};
use twizzler_runtime_api::{AuxEntry, MapFlags};

fn find_init_name(name: &str) -> Option<ObjID> {
    let init_info = twizzler_abi::runtime::get_kernel_init_info();
    for n in init_info.names() {
        if n.name() == name {
            return Some(n.id());
        }
    }
    None
}

fn start_runtime(_runtime_monitor: ObjID, _runtime_library: ObjID) -> ! {
    //miette::set_hook(Box::new(|_| Box::new(miette::DebugReportHandler::new()))).unwrap();
    let engine = Engine;
    let mut ctx = dynlink::context::Context::new(engine);
    let unlib = UnloadedLibrary::new("libmonitor.so");
    let monitor_comp_id = ctx.add_compartment("monitor").unwrap();

    let monitor_id = ctx
        .load_library_in_compartment(monitor_comp_id, unlib, |mut name| {
            if name.starts_with("libstd") {
                name = "libstd.so";
            }
            let id = find_init_name(name)?;
            let obj = twizzler_runtime_api::get_runtime()
                .map_object(id.as_u128(), MapFlags::READ)
                .ok()?;
            Some(Backing::new(obj))
        })
        .unwrap();

    ctx.relocate_all(monitor_id).unwrap();

    let monitor_compartment = ctx.get_compartment_mut(monitor_comp_id).unwrap();
    let tls = monitor_compartment
        .build_tls_region((), |layout| unsafe {
            std::ptr::NonNull::new(std::alloc::alloc_zeroed(layout))
        })
        .unwrap();

    debug!("context loaded, prepping jump to monitor");
    let entry = ctx
        .lookup_symbol(
            monitor_id,
            "monitor_entry_from_bootstrap",
            LookupFlags::empty(),
        )
        .unwrap();

    let value = entry.reloc_value() as usize;
    let ptr: extern "C" fn(usize) = unsafe { core::mem::transmute(value) };

    let mut info = ctx.build_runtime_info(monitor_id, tls).unwrap();
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
    debug!("jumping to {:x}", value);
    (ptr)(aux_ptr as usize);

    warn!("returned from monitor, exiting...");
    exit(0);
}

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
