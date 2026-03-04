use std::{
    ffi::{c_char, c_void, CStr},
    path::Path,
};

use monitor_api::{CompartmentLoader, NewCompartmentFlags};
use twizzler_abi::object::ObjID;
use twizzler_rt_abi::{
    bindings::{descriptor, object_bind_info},
    error::{NamingError, TwzError},
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

fn find_id(name: impl AsRef<str>) -> Result<ObjID, TwzError> {
    let path = Path::new(name.as_ref());
    if path.is_absolute() {
        return twizzler_rt_abi::fd::twz_rt_resolve_name(Default::default(), &name);
    }
    let Ok(candidates) = std::env::var("PATH") else {
        return twizzler_rt_abi::fd::twz_rt_resolve_name(Default::default(), &name);
    };
    let candidates = candidates.split(":");
    for dir in candidates {
        let mut dir = Path::new(dir).to_path_buf();
        dir.push(path);

        if let Ok(r) =
            twizzler_rt_abi::fd::twz_rt_resolve_name(Default::default(), dir.to_str().unwrap())
        {
            return Ok(r);
        }
    }

    Err(NamingError::NotFound.into())
}

impl ReferenceRuntime {
    pub fn exec_spawn(
        &self,
        args: &twizzler_rt_abi::bindings::exec_spawn_args,
    ) -> Result<descriptor, TwzError> {
        let name_cstr = unsafe { CStr::from_ptr(args.prog) };
        let name = name_cstr.to_string_lossy();

        let id = find_id(&name)?;

        let mut loader = CompartmentLoader::new(&name, &name, id, NewCompartmentFlags::empty());

        let progargs = c_str_array_to_vec(args.args);
        let progenv = if args.env.is_null() {
            std::env::vars()
                .map(|(k, v)| format!("{}={}", k, v))
                .collect()
        } else {
            c_str_array_to_vec(args.env)
        };
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
