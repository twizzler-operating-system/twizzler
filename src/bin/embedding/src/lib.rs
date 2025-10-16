#![no_std]

#[macro_use]
extern crate alloc;

use alloc::string::ToString;
use anyhow::Result;
use core::ptr;
use wasmtime::{Engine, Instance, Linker, Module, Store};

/// Entrypoint of this embedding.
///
/// This takes a number of parameters which are the precompiled module AOT
/// images that are run for each of the various tests below. The first parameter
/// is also where to put an error string, if any, if anything fails.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn run(
    error_buf: *mut u8,
    error_size: usize,
    smoke_module: *const u8,
    smoke_size: usize,
    simple_add_module: *const u8,
    simple_add_size: usize,
    simple_host_fn_module: *const u8,
    simple_host_fn_size: usize,
) -> usize {
    unsafe {
        let buf = core::slice::from_raw_parts_mut(error_buf, error_size);
        let smoke = core::slice::from_raw_parts(smoke_module, smoke_size);
        let simple_add = core::slice::from_raw_parts(simple_add_module, simple_add_size);
        let simple_host_fn =
            core::slice::from_raw_parts(simple_host_fn_module, simple_host_fn_size);
        match run_result(smoke, simple_add, simple_host_fn) {
            Ok(()) => 0,
            Err(e) => {
                let msg = format!("{e:?}");
                let len = buf.len().min(msg.len());
                buf[..len].copy_from_slice(&msg.as_bytes()[..len]);
                len
            }
        }
    }
}

fn run_result(
    smoke_module: &[u8],
    simple_add_module: &[u8],
    simple_host_fn_module: &[u8],
) -> Result<()> {
    smoke(smoke_module)?;
    simple_add(simple_add_module)?;
    simple_host_fn(simple_host_fn_module)?;
    Ok(())
}

fn smoke(module: &[u8]) -> Result<()> {
    let engine = Engine::default();
    let module = match deserialize(&engine, module)? {
        Some(module) => module,
        None => return Ok(()),
    };
    Instance::new(&mut Store::new(&engine, ()), &module, &[])?;
    Ok(())
}

fn simple_add(module: &[u8]) -> Result<()> {
    let engine = Engine::default();
    let module = match deserialize(&engine, module)? {
        Some(module) => module,
        None => return Ok(()),
    };
    let mut store = Store::new(&engine, ());
    let instance = Linker::new(&engine).instantiate(&mut store, &module)?;
    let func = instance.get_typed_func::<(u32, u32), u32>(&mut store, "add")?;
    assert_eq!(func.call(&mut store, (2, 3))?, 5);
    Ok(())
}

fn simple_host_fn(module: &[u8]) -> Result<()> {
    let engine = Engine::default();
    let module = match deserialize(&engine, module)? {
        Some(module) => module,
        None => return Ok(()),
    };
    let mut linker = Linker::<()>::new(&engine);
    linker.func_wrap("host", "multiply", |a: u32, b: u32| a.saturating_mul(b))?;
    let mut store = Store::new(&engine, ());
    let instance = linker.instantiate(&mut store, &module)?;
    let func = instance.get_typed_func::<(u32, u32, u32), u32>(&mut store, "add_and_mul")?;
    assert_eq!(func.call(&mut store, (2, 3, 4))?, 10);
    Ok(())
}

fn deserialize(engine: &Engine, module: &[u8]) -> Result<Option<Module>> {
    // NOTE: deserialize_raw avoids creating a copy of the module code.  See the
    // safety notes before using in your embedding.
    let memory_ptr = ptr::slice_from_raw_parts(module.as_ptr(), module.len());
    let module_memory = ptr::NonNull::new(memory_ptr.cast_mut()).unwrap();
    match unsafe { Module::deserialize_raw(engine, module_memory) } {
        Ok(module) => Ok(Some(module)),
        Err(e) => {
            // Currently if custom signals/virtual memory are disabled then this
            // example is expected to fail to load since loading native code
            // requires virtual memory. In the future this will go away as when
            // signals-based-traps is disabled then that means that the
            // interpreter should be used which should work here.
            if !cfg!(feature = "custom")
                && e.to_string()
                    .contains("requires virtual memory to be enabled")
            {
                Ok(None)
            } else {
                Err(e)
            }
        }
    }
}


#[cfg(target_os = "twizzler")]
fn main() -> Result<()> {
    use anyhow::{Context, anyhow};
    use std::io::Write;
    use wasmtime::{Config, Engine};

    // Precompile modules for the embedding. Right now Wasmtime in no_std mode
    // does not have support for Cranelift meaning that AOT mode must be used.
    // Modules are compiled here and then given to the embedding via the `run`
    // function below.
    //
    // Note that `Config::target` is used here to enable cross-compilation.
    let triple = "aarch64-unknown-twizzler";
    let mut config = Config::new();
    config.target(&triple)?;

    // If signals-based-traps are disabled then that additionally means that
    // some configuration knobs need to be turned to match the expectations of
    // the guest program being loaded.
    // if !cfg!(feature = "custom") {
        config.memory_init_cow(false);
        config.memory_reservation(0);
        config.memory_guard_size(0);
        config.memory_reservation_for_growth(0);
        config.signals_based_traps(false);
    // }

    let engine = Engine::new(&config)?;
    let smoke = engine.precompile_module(b"(module)")?;
    let simple_add = engine.precompile_module(
        br#"
            (module
                (func (export "add") (param i32 i32) (result i32)
                    (i32.add (local.get 0) (local.get 1)))
            )
        "#,
    )?;
    let simple_host_fn = engine.precompile_module(
        br#"
            (module
                (import "host" "multiply" (func $multiply (param i32 i32) (result i32)))
                (func (export "add_and_mul") (param i32 i32 i32) (result i32)
                    (i32.add (call $multiply (local.get 0) (local.get 1)) (local.get 2)))
            )
        "#,
    )?;

    // Next is an example of running this embedding, which also serves as test
    // that basic functionality actually works.
    //
    // Here the `wasmtime_*` symbols are implemented by
    // `./embedding/wasmtime-platform.c` which is an example implementation
    // against glibc on Linux. This library is compiled into
    // `libwasmtime-platform.so` and is dynamically opened here to make it
    // available for later symbol resolution. This is just an implementation
    // detail of this exable to enably dynamically loading `libembedding.so`
    // next.
    //
    // Next the `libembedding.so` library is opened and the `run` symbol is
    // run. The dependencies of `libembedding.so` are either satisfied by our
    // ambient libc (e.g. `memcpy` and friends) or `libwasmtime-platform.so`
    // (e.g. `wasmtime_*` symbols).
    //
    // The embedding is then run to showcase an example and then an error, if
    // any, is written to stderr.
    unsafe {
        let mut error_buf = Vec::with_capacity(1024);
        let len = run(
            error_buf.as_mut_ptr(),
            error_buf.capacity(),
            smoke.as_ptr(),
            smoke.len(),
            simple_add.as_ptr(),
            simple_add.len(),
            simple_host_fn.as_ptr(),
            simple_host_fn.len(),
        );
        error_buf.set_len(len);

        std::io::stderr().write_all(&error_buf).unwrap();
    }
    Ok(())
}
