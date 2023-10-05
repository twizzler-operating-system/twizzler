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
    DynlinkError,
};
use tracing::Level;
use tracing_subscriber::FmtSubscriber;
use twizzler_abi::{
    object::ObjID,
    syscall::{sys_object_create, ObjectCreateError, ObjectSource},
};

fn create_obj(copy_cmds: &[ObjectSource]) -> Result<ObjID, ObjectCreateError> {
    let create_spec = ObjectCreate::new(
        BackingType::Normal,
        LifetimeType::Volatile,
        None,
        ObjectCreateFlags::empty(),
    );
    let id = sys_object_create(create_spec, &copy_cmds, &[])?;
    Ok(id)
}

fn map_objs(data_id: ObjID, text_id: ObjID) -> Result<(Object<u8>, Object<u8>), ObjectInitError> {
    let data = Object::init_id(
        data_id,
        Protections::READ | Protections::WRITE,
        ObjectInitFlags::empty(),
    )?;

    let text = Object::init_id(
        text_id,
        Protections::READ | Protections::EXEC,
        ObjectInitFlags::empty(),
    )?;
    Ok((data, text))
}

fn start_runtime(_exec_id: ObjID, runtime_monitor: ObjID, runtime_library: ObjID, libstd: ObjID) {
    let mut ctx = dynlink::context::Context::default();
    let mut monitor_compartment = ctx.add_compartment("monitor").unwrap();

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
    let _roots = ctx
        .relocate_all([monitor.clone(), runtime], &mut loader)
        .unwrap();
    //ctx.add_library(&monitor_compartment, libstd_library, &mut loader)
    //    .unwrap();

    eprintln!("== Context Ready, Building Arguments ==");

    eprintln!("== Jumping to Monitor ==");
    let entry = ctx
        .lookup_symbol(&monitor, "monitor_entry_from_bootstrap")
        .unwrap();

    let value = entry.reloc_value() as usize;
    eprintln!("==> Jumping to {:x}", value);
    let ptr: extern "C" fn() = unsafe { core::mem::transmute(value) };
    (ptr)();
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

    let exec_id = find_init_name("libhello_world.so").unwrap();
    let runtime_lib = find_init_name("libtwz_rt.so").unwrap();
    let monitor = find_init_name("libmonitor.so").unwrap();
    let libstd = find_init_name("libstd.so").unwrap();

    eprintln!("=== BOOTSTRAP RUNTIME ===");
    start_runtime(exec_id, monitor, runtime_lib, libstd);

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
use twizzler_object::{Object, ObjectInitError, ObjectInitFlags};
