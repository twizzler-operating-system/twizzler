use std::{
    ffi::{c_char, c_void, CStr},
    path::{Path, PathBuf},
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
    // POSIX: a command name containing a slash is a *path* -- absolute, or relative to the working
    // directory -- and is never looked for on PATH; only a bare name searches PATH. Without the
    // slash test, `./prog` was searched as `<pathdir>/./prog` for each PATH entry and so could not
    // be run at all. A relative name resolves against this compartment's working namespace, which
    // is the naming server's, so it means the same directory the shell's prompt is showing.
    if path.is_absolute() || name.as_ref().contains('/') {
        return twizzler_rt_abi::fd::twz_rt_resolve_name(Default::default(), &name);
    }
    let Ok(candidates) = std::env::var("PATH") else {
        return twizzler_rt_abi::fd::twz_rt_resolve_name(Default::default(), &name);
    };
    // Entries are expanded one at a time, in order, rather than all of them into a list up front:
    // a name that hits on `/initrd` -- the first entry, and where everything in the boot image
    // lives -- must not pay for enumerating `/pkg`. This is on the spawn path, so that cost would
    // land on every program launch, hit or miss.
    for entry in candidates.split(':') {
        match expand_star(Path::new(entry)) {
            Some(expanded) => {
                for dir in expanded {
                    if let Some(id) = try_dir(&dir, path) {
                        return Ok(id);
                    }
                }
            }
            None => {
                if let Some(id) = try_dir(Path::new(entry), path) {
                    return Ok(id);
                }
            }
        }
    }

    Err(NamingError::NotFound.into())
}

fn try_dir(dir: &Path, name: &Path) -> Option<ObjID> {
    let candidate = dir.join(name);
    twizzler_rt_abi::fd::twz_rt_resolve_name(Default::default(), candidate.to_str()?).ok()
}

/// Expands `<prefix>/*/<suffix>` into one path per entry of `<prefix>`.
///
/// `/pkg` gains a directory whenever a package is installed, so the set of program directories is
/// not known when `init` sets PATH -- `/pkg/*/bin` names whatever is installed at the moment of
/// the lookup instead. brush carries the same expansion for its own command search
/// (`sys/twizzler/fs.rs`); this one covers everything else, i.e. any program spawned by bare name
/// through the runtime, which is how rustc finds `ld.lld`.
///
/// `None` means "not a glob, use the entry as it is": both for an entry with no `*`, and for one
/// whose prefix cannot be enumerated -- a missing prefix is what a PATH entry naming an
/// uninstalled package already looked like, and the resolve attempt above handles it.
///
/// The expansion is not narrowed to directories that exist. That check is a naming call per
/// package on every lookup, whereas an expansion naming a missing directory costs one failed
/// resolve, and only on a lookup that gets that far.
fn expand_star(entry: &Path) -> Option<Vec<PathBuf>> {
    let mut components = entry.components();
    let mut prefix = PathBuf::new();
    loop {
        let component = components.next()?;
        if component.as_os_str() == "*" {
            break;
        }
        prefix.push(component);
    }
    let suffix: PathBuf = components.collect();

    // Enumerated through the naming handle rather than `std::fs::read_dir`, which would re-enter
    // this crate through the `twz_rt_fd_*` symbols it exports.
    let nsid =
        twizzler_rt_abi::fd::twz_rt_resolve_name(Default::default(), prefix.to_str()?).ok()?;
    let session = crate::runtime::file::get_naming_handle()?;
    // `usize::MAX` is "all of them": the handle pages through its buffer and stops early, so this
    // asks for everything rather than silently truncating at some cap.
    let names = session.enumerate_names_nsid(nsid, 0, usize::MAX).ok()?;

    let mut expanded: Vec<PathBuf> = names
        .iter()
        .filter_map(|node| node.name().ok())
        .map(|name| prefix.join(name).join(&suffix))
        .collect();
    // Enumeration order is not stable across boots, and PATH order is what decides which of two
    // same-named programs runs.
    expanded.sort();
    Some(expanded)
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

        // A pipe object the child receives as stdio must not also arrive at fd>2: the extra
        // reference holds the pipe's reader/writer counts up, so EOF-on-close never fires
        // (spawn-test's stdin-pipe-eof deadlocked on the child holding the write end of its
        // own stdin). The extras exist because the toolchain std's anon_pipe predates the
        // cloexec fix in library/std/src/sys/pipe/unix.rs; this drop restores the semantics
        // pipe2(O_CLOEXEC) intends, and is deliberately narrow -- only Pipe binds duplicating
        // a stdio Pipe's *object* are dropped, so a jobserver-style deliberate pipe pass
        // (never stdio-duplicated) survives. Harmlessly redundant once the std fix ships.
        let pipe_obj = |b: &twizzler_rt_abi::bindings::binding_info| -> Option<u128> {
            if !matches!(OpenKind::try_from(b.kind), Ok(OpenKind::Pipe)) {
                return None;
            }
            if (b.bind_len as usize) < size_of::<object_bind_info>() {
                return None;
            }
            Some(
                unsafe {
                    b.bind_data
                        .as_ptr()
                        .cast::<object_bind_info>()
                        .read_unaligned()
                }
                .id,
            )
        };
        let stdio_pipes: Vec<u128> = bindings
            .iter()
            .filter(|b| b.fd <= 2)
            .filter_map(&pipe_obj)
            .collect();
        let filtered: Vec<twizzler_rt_abi::bindings::binding_info> = bindings
            .iter()
            .filter(|b| {
                let drop = b.fd > 2 && pipe_obj(b).is_some_and(|id| stdio_pipes.contains(&id));
                // Fires on every piped spawn, so the audit line is opt-in (`--diag=exec`); the
                // drop itself is the validated behavior and stays.
                if drop && twizzler_net::diag_enabled("exec") {
                    twizzler_abi::klog_println!("SPAWNDROP {} fd={} (stdio pipe dup)", name, b.fd);
                }
                !drop
            })
            .cloned()
            .collect();

        loader.with_fd_specs(&filtered);

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
