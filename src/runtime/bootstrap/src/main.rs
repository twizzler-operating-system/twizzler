use dynlink::{compartment::Compartment, library::UnloadedLibrary};
use twizzler_abi::object::ObjID;

fn start_runtime(exec_id: ObjID, runtime_monitor: ObjID, runtime_library: ObjID, libstd: ObjID) {
    let ctx = dynlink::context::Context::default();
    let mut monitor_compartment = dynlink::compartment::UnloadedCompartment::default();
    let mc_id = monitor_compartment.id();

    monitor_compartment
        .add_library(UnloadedLibrary::new(runtime_monitor, mc_id, "monitor").unwrap())
        .unwrap();

    monitor_compartment
        .add_library(UnloadedLibrary::new(runtime_library, mc_id, "runtime").unwrap())
        .unwrap();

    monitor_compartment
        .add_library(UnloadedLibrary::new(libstd, mc_id, "libstd").unwrap())
        .unwrap();
}

fn main() {
    println!("Hello, world!");
    let _runtime = twizzler_abi::runtime::__twz_get_runtime();
}
