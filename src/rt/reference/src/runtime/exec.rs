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
        tracing::trace!(
            "DOING EXEC SPAWN: prog={:?}, args={:?}, env={:?}",
            unsafe { CStr::from_ptr(args.prog) },
            c_str_array_to_vec(args.args),
            c_str_array_to_vec(args.env)
        );
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
        // libstd's `Command::spawn` has no channel for an explicit `current_dir()` other than the
        // environment, so it sets `TWZ_RT_INITIAL_DIR` there. Consume it here and *remove* it: the
        // compartment config carries the value now, and a copy left in the environment is what
        // let a stale directory ride along into grandchildren, since `std::env::vars()` inherits
        // the whole block. Only honour it from an explicitly supplied environment -- when `env` is
        // null the vars are this compartment's own, where that variable is a leftover from our own
        // startup and names the wrong directory.
        let explicit_env = !args.env.is_null();
        let mut initial_cwd = None;
        let progenv: Vec<String> = progenv
            .into_iter()
            .filter(|kv| match kv.strip_prefix("TWZ_RT_INITIAL_DIR=") {
                Some(dir) => {
                    if explicit_env {
                        initial_cwd = Some(dir.to_string());
                    }
                    false
                }
                None => true,
            })
            .collect();

        let bindings = unsafe { core::slice::from_raw_parts(args.fd_binds, args.fd_bind_count) };

        loader.with_fd_specs(bindings);

        // Inheriting a working directory and being sent to one are different things, and only the
        // second is really a name. If the requested directory is the one we are already in, hand
        // over a *bequest*: the naming server keeps our actual working namespace with its parent
        // chain intact, so the child's `getcwd` reports what ours would and a rename racing the
        // spawn cannot move it. An explicit redirect elsewhere has no such state to borrow and
        // travels as a path.
        let here = crate::runtime::file::current_dir().ok();
        let inherits = match (&initial_cwd, &here) {
            (Some(dir), Some(here)) => Path::new(dir) == here.as_path(),
            // No explicit request: inherit.
            (None, _) => true,
            _ => false,
        };
        match (inherits, &initial_cwd) {
            (true, _) => {
                if let Some(token) = crate::runtime::file::mint_cwd_bequest() {
                    loader.with_initial_cwd_token(token);
                }
            }
            (false, Some(cwd)) => {
                loader.with_initial_cwd(cwd.as_bytes());
            }
            (false, None) => {}
        }
        loader.args(progargs);
        loader.env(progenv);

        let comp = loader.load()?;
        let id = comp.info()?.id.raw();
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
