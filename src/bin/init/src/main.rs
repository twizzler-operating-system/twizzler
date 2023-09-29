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
    compartment::{Compartment, LibraryResolver},
    library::{LibraryLoader, UnloadedLibrary},
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
    let mut monitor_compartment = ctx.new_compartment("monitor");
    let mc_id = monitor_compartment.id();

    monitor_compartment
        .add_library(UnloadedLibrary::new(&mut ctx, runtime_monitor, mc_id, "monitor").unwrap())
        .unwrap();

    monitor_compartment
        .add_library(UnloadedLibrary::new(&mut ctx, runtime_library, mc_id, "runtime").unwrap())
        .unwrap();

    let libstd_lib = UnloadedLibrary::new(&mut ctx, libstd, mc_id, "libstd").unwrap();
    monitor_compartment.add_library(libstd_lib.clone()).unwrap();

    let mut lib_resolver = LibraryResolver::new(Box::new(move |n| {
        println!("==> res {:?}", n);
        if String::from_utf8_lossy(n.0).starts_with("libstd") {
            Ok(libstd_lib.clone())
        } else {
            Err(dynlink::LookupError::NotFound)
        }
    }));

    let mut lib_loader = LibraryLoader::new(
        Box::new(move |_data, cmds| create_obj(cmds)),
        Box::new(move |data_id, text_id| map_objs(data_id, text_id)),
    );

    ctx.add_compartment(monitor_compartment, &mut lib_resolver, &mut lib_loader)
        .unwrap();
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
