use std::ffi::{c_char, c_void, CStr};

use monitor_api::{CompartmentLoader, NewCompartmentFlags};
use twizzler_rt_abi::{
    bindings::{descriptor, object_bind_info},
    error::TwzError,
    fd::OpenKind,
};

use crate::runtime::{file::OperationOptions, ReferenceRuntime};

fn c_str_array_to_vec(arr: *const *const c_char) -> Vec<String> {
    if arr.is_null() {
        return Vec::new();
    }

    let mut vec = Vec::new();
    let mut ptr = arr;
    while !unsafe { (*ptr).is_null() } {
        let c_str = unsafe { CStr::from_ptr(*ptr) };
        vec.push(c_str.to_string_lossy().to_string());
        ptr = unsafe { ptr.offset(1) };
    }
    vec
}

impl ReferenceRuntime {
    pub fn exec_spawn(
        &self,
        args: &twizzler_rt_abi::bindings::exec_spawn_args,
    ) -> Result<descriptor, TwzError> {
        let name_cstr = unsafe { CStr::from_ptr(args.prog) };
        let name = name_cstr.to_string_lossy();
        let mut loader = CompartmentLoader::new(&name, &name, NewCompartmentFlags::empty());

        let progargs = c_str_array_to_vec(args.args);
        let progenv = c_str_array_to_vec(args.env);
        let bindings = unsafe { core::slice::from_raw_parts(args.fd_binds, args.fd_bind_count) };

        loader.with_fd_specs(bindings);
        loader.args(progargs);
        loader.env(progenv);

        let comp = loader.load()?;
        let id = comp.info().id.raw();
        let bind_info = object_bind_info { id };

        self.open(
            None,
            OpenKind::Compartment,
            OperationOptions::OPEN_FLAG_READ,
            &bind_info as *const _ as *const c_void,
            size_of::<object_bind_info>(),
            true,
        )
    }
}
