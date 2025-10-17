use anyhow::Result;

// #[cfg(not(target_os = "linux"))]
// fn main() -> Result<()> {
//     eprintln!("This example only runs on Linux right now");
//     Ok(())
// }

#[cfg(target_os = "twizzler")]
fn main() -> Result<()> {
    // use anyhow::{Context, anyhow};
    use std::io::Write;
    use wasmtime::{Config, Engine};

    // Precompile modules for the embedding. Right now Wasmtime in no_std mode
    // does not have support for Cranelift meaning that AOT mode must be used.
    // Modules are compiled here and then given to the embedding via the `run`
    // function below.
    //
    // Note that `Config::target` is used here to enable cross-compilation.
    // std::io::stderr().write_all(&error_buf).unwrap();
    eprintln!("This is going to standard error!, {}", "awesome");

    let triple = "aarch64-unknown-linux";
    let mut config = Config::new();
    // config.target(&triple)?;

    eprintln!("Here!, {}", "awesome");
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
    eprintln!("Now here!, {}", "awesome");
    let smoke = engine.precompile_module(b"(module)")?;
    eprintln!("And here!, {}", "awesome");
    let simple_add = engine.precompile_module(
        br#"
            (module
                (func (export "add") (param i32 i32) (result i32)
                    (i32.add (local.get 0) (local.get 1)))
            )
        "#,
    )?;
    eprintln!("And also here!, {}", "awesome");
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
        let len = twasm::run(
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
