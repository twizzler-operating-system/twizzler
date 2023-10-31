#![allow(unreachable_code)]

fn find_init_name(name: &str) -> Option<ObjID> {
    let init_info = twizzler_abi::runtime::get_kernel_init_info();
    for n in init_info.names() {
        if n.name() == name {
            return Some(n.id());
        }
    }
    None
}

use dynlink::{
    library::{Library, LibraryLoader},
    symbol::LookupFlags,
    DynlinkError,
};
use tracing::Level;
use tracing_subscriber::FmtSubscriber;
use twizzler_abi::{
    object::ObjID,
    syscall::{sys_object_create, ObjectSource},
};

fn start_runtime(runtime_monitor: ObjID, runtime_library: ObjID, libstd: ObjID) {
    let ctx = dynlink::context::Context::default();
    let monitor_compartment = ctx.add_compartment("monitor").unwrap();

    let mon_library = Library::new(
        Object::<u8>::init_id(runtime_monitor, Protections::READ, ObjectInitFlags::empty())
            .unwrap(),
        "monitor",
    );

    let rt_library = Library::new(
        Object::<u8>::init_id(runtime_library, Protections::READ, ObjectInitFlags::empty())
            .unwrap(),
        "runtime",
    );

    let _libstd_library = Library::new(
        Object::<u8>::init_id(libstd, Protections::READ, ObjectInitFlags::empty()).unwrap(),
        "libstd",
    );

    let mut loader = Loader {};
    let monitor = ctx
        .add_library(&monitor_compartment, mon_library, &mut loader)
        .unwrap();
    let runtime = ctx
        .add_library(&monitor_compartment, rt_library, &mut loader)
        .unwrap();
    let roots = ctx.relocate_all([monitor.clone(), runtime]).unwrap();
    //ctx.add_library(&monitor_compartment, libstd_library, &mut loader)
    //    .unwrap();

    let tls = monitor_compartment.build_tls_region(()).unwrap();

    eprintln!("== Context Ready, Building Arguments ==");

    eprintln!("== Jumping to Monitor ==");
    let entry = ctx
        .lookup_symbol(
            &monitor,
            "monitor_entry_from_bootstrap",
            LookupFlags::empty(),
        )
        .unwrap();

    let value = entry.reloc_value() as usize;
    eprintln!("==> Jumping to {:x}", value);
    let ptr: extern "C" fn(usize) = unsafe { core::mem::transmute(value) };

    let info = ctx.build_runtime_info(roots, tls).unwrap();
    let info_ptr = &info as *const _ as usize;
    let aux = vec![AuxEntry::RuntimeInfo(info_ptr), AuxEntry::Null];
    let aux_ptr = aux.as_slice().as_ptr();
    (ptr)(aux_ptr as usize);
}

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

fn main() {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::DEBUG)
        .finish();

    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    //let exec_id = find_init_name("libhello_world.so").unwrap();
    let runtime_lib = find_init_name("libtwz_rt.so").unwrap();
    let monitor = find_init_name("libmonitor.so").unwrap();
    let libstd = find_init_name("libstd.so").unwrap();

    eprintln!("=== BOOTSTRAP RUNTIME ===");
    start_runtime(monitor, runtime_lib, libstd);

    let _runtime = twizzler_abi::runtime::__twz_get_runtime();
}

use twizzler_abi::{
    object::Protections,
    syscall::{
        BackingType,
        LifetimeType, //MapFlags,
        ObjectCreate,
        ObjectCreateFlags,
    },
};
use twizzler_object::{Object, ObjectInitFlags};
use twizzler_runtime_api::AuxEntry;
