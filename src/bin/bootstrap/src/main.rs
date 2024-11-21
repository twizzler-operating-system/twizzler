use std::process::exit;

use dynlink::{
    compartment::MONITOR_COMPARTMENT_ID,
    context::runtime::RuntimeInitInfo,
    engines::{Backing, ContextEngine},
    library::UnloadedLibrary,
    symbol::LookupFlags,
    DynlinkError, DynlinkErrorKind,
};
use tracing::{debug, info, warn, Level};
use tracing_subscriber::FmtSubscriber;
use twizzler_abi::{arch::SLOTS, object::ObjID, syscall::sys_object_read_map};
use twizzler_rt_abi::{
    core::{InitInfoPtrs, RuntimeInfo},
    object::MapFlags,
};

fn find_init_name(name: &str) -> Option<ObjID> {
    let init_info = twizzler_abi::runtime::get_kernel_init_info();
    for n in init_info.names() {
        if n.name() == name {
            return Some(n.id());
        }
    }
    None
}

struct Engine;

impl ContextEngine for Engine {
    fn load_segments(
        &mut self,
        src: &Backing,
        ld: &[dynlink::engines::LoadDirective],
    ) -> Result<Vec<Backing>, dynlink::DynlinkError> {
        dynlink::engines::twizzler::load_segments(src, ld)
    }

    fn load_object(&mut self, unlib: &UnloadedLibrary) -> Result<Backing, DynlinkError> {
        let id = name_resolver(&unlib.name)?;
        Ok(Backing::new(
            twizzler_rt_abi::object::twz_rt_map_object(id, MapFlags::READ)
                .map_err(|_err| DynlinkErrorKind::NewBackingFail)?,
        ))
    }

    fn select_compartment(
        &mut self,
        _unlib: &UnloadedLibrary,
    ) -> Option<dynlink::compartment::CompartmentId> {
        Some(MONITOR_COMPARTMENT_ID)
    }
}

fn name_resolver(mut name: &str) -> Result<ObjID, DynlinkError> {
    if name.starts_with("libstd") {
        name = "libstd.so";
    }
    find_init_name(name).ok_or(
        DynlinkErrorKind::NameNotFound {
            name: name.to_string(),
        }
        .into(),
    )
}

fn start_runtime(_runtime_monitor: ObjID, _runtime_library: ObjID) -> ! {
    //miette::set_hook(Box::new(|_| Box::new(miette::DebugReportHandler::new()))).unwrap();
    let engine = Engine;
    let mut ctx = dynlink::context::Context::new(Box::new(engine));
    let unlib = UnloadedLibrary::new("libmonitor.so");
    let monitor_comp_id = ctx.add_compartment("monitor").unwrap();

    let monitor_id = ctx
        .load_library_in_compartment(monitor_comp_id, unlib)
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
    let info_ptr = &mut info as *mut RuntimeInitInfo;
    let mut rtinfo = RuntimeInfo {
        flags: 0,
        kind: twizzler_rt_abi::core::RUNTIME_INIT_MONITOR,
        init_info: InitInfoPtrs {
            monitor: info_ptr.cast(),
        },
    };
    let rtinfo_ptr = &mut rtinfo as *mut RuntimeInfo;

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

    debug!("jumping to {:x}", value);
    (ptr)(rtinfo_ptr as usize);

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
