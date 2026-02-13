//! Wasmtime WebAssembly runtime for Twizzler.
//!
//! Usage:
//!   wasmrun                — run built-in wasmtime demos + WASI tests
//!   wasmrun mandelbrot     — run interactive Mandelbrot (ANSI terminal)
//!   wasmrun mandelbrot-gfx — run graphical Mandelbrot auto-zoom (WASI-GFX)
//!   wasmrun test           — run comprehensive WASI P2 test suite
//!   wasmrun test-net       — test UDP sockets and DNS resolution
//!   wasmrun test-rename    — test file/namespace rename operations
//!   wasmrun <path.wasm>    — run a WASI P1 module or P2 component from file

// Pull in the platform callbacks so they are linked into the binary.
mod net;
mod platform;
mod wasi;
mod wasi_p1;

use anyhow::Result;
use wasmtime::{Config, Engine, Instance, Linker, Module, Store};

/// Embedded WASI hello-world component for testing without filesystem access.
const HELLO_WASI: &[u8] = include_bytes!("../hello.wasm");

/// Embedded comprehensive WASI P2 test suite.
const WASI_TESTS: &[u8] = include_bytes!("../wasi_tests.wasm");

/// Embedded Mandelbrot set renderer (ANSI 256-color, terminal).
const MANDELBROT: &[u8] = include_bytes!("../mandelbrot.wasm");

/// Embedded graphical Mandelbrot auto-zoom (WASI-GFX, display).
const MANDELBROT_GFX: &[u8] = include_bytes!("../mandelbrot_gfx.wasm");

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(|s| s.as_str());

    match cmd {
        Some("mandelbrot") => {
            match wasi::run_wasi_component(MANDELBROT) {
                Ok(()) => {}
                Err(e) => eprintln!("Error: {e:?}"),
            }
        }
        Some("mandelbrot-gfx") => {
            match wasi::run_wasi_component(MANDELBROT_GFX) {
                Ok(()) => {}
                Err(e) => eprintln!("Error: {e:?}"),
            }
        }
        Some("test") => {
            println!("=== Wasmtime WASI Test Suite ===");
            println!();
            match wasi::run_wasi_component(WASI_TESTS) {
                Ok(()) => {}
                Err(e) => eprintln!("Error: {e:?}"),
            }
        }
        Some("test-net") => {
            println!("=== Network Tests (UDP + DNS) ===");
            println!();
            match test_net() {
                Ok(()) => println!("\nAll network tests passed!"),
                Err(e) => eprintln!("\nNetwork test FAILED: {e:?}"),
            }
        }
        Some("test-rename") => {
            println!("=== Rename Tests ===");
            println!();
            match test_rename() {
                Ok(()) => println!("\nAll rename tests passed!"),
                Err(e) => eprintln!("\nRename test FAILED: {e:?}"),
            }
        }
        Some(path) => {
            // Run a WASI module/component from a file path (auto-detect P1 vs P2).
            println!("=== Wasmtime WASI on Twizzler ===");
            println!("Loading: {path}");
            match std::fs::read(path) {
                Ok(bytes) => {
                    let result = if wasi_p1::is_component(&bytes) {
                        println!("Detected: WASI P2 component");
                        wasi::run_wasi_component(&bytes)
                    } else {
                        println!("Detected: WASI P1 module");
                        wasi_p1::run_wasi_p1_module(&bytes)
                    };
                    match result {
                        Ok(()) => println!("Module exited successfully."),
                        Err(e) => eprintln!("Error: {e:?}"),
                    }
                }
                Err(e) => eprintln!("Failed to read {path}: {e}"),
            }
        }
        None => {
            // Run built-in demos + embedded WASI hello world.
            println!("=== Wasmtime on Twizzler ===");
            println!();
            match run_all() {
                Ok(()) => {
                    println!();
                    println!("All tests passed!");
                }
                Err(e) => {
                    eprintln!();
                    eprintln!("Error: {e:?}");
                }
            }
        }
    }
}

fn run_all() -> Result<()> {
    run_demos()?;
    demo_wasi()?;
    Ok(())
}

/// Build a wasmtime Config suitable for Twizzler.
fn wasmtime_config() -> Config {
    let mut config = Config::new();
    config.memory_init_cow(false);
    config.memory_reservation(0);
    config.memory_guard_size(0);
    config.memory_reservation_for_growth(0);
    config.signals_based_traps(false);
    config
}

fn run_demos() -> Result<()> {
    demo_smoke()?;
    demo_add()?;
    demo_host_fn()?;
    Ok(())
}

/// Demo 1: Instantiate an empty WASM module (smoke test).
fn demo_smoke() -> Result<()> {
    println!("[1/3] Smoke test: instantiating empty module...");

    let engine = Engine::new(&wasmtime_config())?;
    let module = Module::new(&engine, "(module)")?;
    let mut store = Store::new(&engine, ());
    Instance::new(&mut store, &module, &[])?;

    println!("      OK");
    Ok(())
}

/// Demo 2: Compile and call a WASM function that adds two integers.
fn demo_add() -> Result<()> {
    println!("[2/3] Simple add: compiling and calling exported WASM function...");

    let engine = Engine::new(&wasmtime_config())?;
    let module = Module::new(
        &engine,
        r#"
        (module
            (func (export "add") (param i32 i32) (result i32)
                (i32.add (local.get 0) (local.get 1))
            )
        )
        "#,
    )?;

    let mut store = Store::new(&engine, ());
    let instance = Linker::new(&engine).instantiate(&mut store, &module)?;
    let add = instance.get_typed_func::<(i32, i32), i32>(&mut store, "add")?;

    let result = add.call(&mut store, (2, 3))?;
    assert_eq!(result, 5, "expected add(2, 3) = 5, got {result}");
    println!("      add(2, 3) = {result}");

    let result = add.call(&mut store, (100, -42))?;
    assert_eq!(result, 58, "expected add(100, -42) = 58, got {result}");
    println!("      add(100, -42) = {result}");

    println!("      OK");
    Ok(())
}

/// Demo 3: WASM module that imports a host-provided "multiply" function,
/// then exports "add_and_mul" which computes `multiply(a, b) + c`.
fn demo_host_fn() -> Result<()> {
    println!("[3/3] Host function: WASM calling back into host code...");

    let engine = Engine::new(&wasmtime_config())?;
    let module = Module::new(
        &engine,
        r#"
        (module
            (import "host" "multiply" (func $multiply (param i32 i32) (result i32)))
            (func (export "add_and_mul") (param i32 i32 i32) (result i32)
                (i32.add
                    (call $multiply (local.get 0) (local.get 1))
                    (local.get 2)
                )
            )
        )
        "#,
    )?;

    let mut linker = Linker::<()>::new(&engine);
    linker.func_wrap("host", "multiply", |a: i32, b: i32| -> i32 {
        a.wrapping_mul(b)
    })?;

    let mut store = Store::new(&engine, ());
    let instance = linker.instantiate(&mut store, &module)?;
    let func = instance.get_typed_func::<(i32, i32, i32), i32>(&mut store, "add_and_mul")?;

    // add_and_mul(2, 3, 4) = multiply(2, 3) + 4 = 6 + 4 = 10
    let result = func.call(&mut store, (2, 3, 4))?;
    assert_eq!(result, 10, "expected add_and_mul(2, 3, 4) = 10, got {result}");
    println!("      add_and_mul(2, 3, 4) = {result}  [multiply(2,3) + 4]");

    // add_and_mul(7, 8, 1) = multiply(7, 8) + 1 = 56 + 1 = 57
    let result = func.call(&mut store, (7, 8, 1))?;
    assert_eq!(result, 57, "expected add_and_mul(7, 8, 1) = 57, got {result}");
    println!("      add_and_mul(7, 8, 1) = {result}  [multiply(7,8) + 1]");

    println!("      OK");
    Ok(())
}

/// Demo 4: Run an embedded WASI P2 component (hello world).
fn demo_wasi() -> Result<()> {
    println!("[4/4] WASI component: running embedded hello-world...");
    wasi::run_wasi_component(HELLO_WASI)?;
    println!("      OK");
    Ok(())
}

/// Test file/namespace rename operations via the runtime ABI.
fn test_rename() -> Result<()> {
    use twizzler_rt_abi::fd;

    // Use a unique namespace to avoid collisions with other data.
    let ns = "/test_rename";
    fd::twz_rt_fd_mkns(ns).map_err(|e| anyhow::anyhow!("mkns {ns}: {e:?}"))?;

    // Test 1: Basic rename of a file
    println!("[1/7] Basic rename: create file, rename, verify...");
    let old = &format!("{ns}/alpha");
    let new = &format!("{ns}/beta");
    // Create a file at old path by opening with CREATE_KIND_NEW.
    let create = twizzler_rt_abi::bindings::create_options {
        id: Default::default(),
        kind: twizzler_rt_abi::bindings::CREATE_KIND_NEW,
    };
    let rw = twizzler_rt_abi::bindings::OPEN_FLAG_READ | twizzler_rt_abi::bindings::OPEN_FLAG_WRITE;
    let f = fd::twz_rt_fd_open(old, create, rw)
        .map_err(|e| anyhow::anyhow!("create {old}: {e:?}"))?;
    fd::twz_rt_fd_close(f);

    fd::twz_rt_fd_rename(old, new).map_err(|e| anyhow::anyhow!("rename {old} -> {new}: {e:?}"))?;
    // Old name should be gone.
    let create_exist = twizzler_rt_abi::bindings::create_options {
        id: Default::default(),
        kind: twizzler_rt_abi::bindings::CREATE_KIND_EXISTING,
    };
    assert!(
        fd::twz_rt_fd_open(old, create_exist, twizzler_rt_abi::bindings::OPEN_FLAG_READ).is_err(),
        "old name should not exist after rename"
    );
    // New name should exist.
    let f2 = fd::twz_rt_fd_open(new, create_exist, twizzler_rt_abi::bindings::OPEN_FLAG_READ)
        .map_err(|e| anyhow::anyhow!("open {new} after rename: {e:?}"))?;
    fd::twz_rt_fd_close(f2);
    println!("      OK");

    // Test 2: Rename to already-existing name should fail
    println!("[2/7] Rename conflict: rename to existing name should fail...");
    let other = &format!("{ns}/gamma");
    let f3 = fd::twz_rt_fd_open(other, create, rw)
        .map_err(|e| anyhow::anyhow!("create {other}: {e:?}"))?;
    fd::twz_rt_fd_close(f3);
    assert!(
        fd::twz_rt_fd_rename(new, other).is_err(),
        "rename to existing name should fail"
    );
    println!("      OK");

    // Test 3: Rename non-existent source should fail
    println!("[3/7] Rename missing: rename non-existent source should fail...");
    assert!(
        fd::twz_rt_fd_rename(&format!("{ns}/nonexistent"), &format!("{ns}/whatever")).is_err(),
        "rename of non-existent source should fail"
    );
    println!("      OK");

    // Test 4: Rename a symlink (moves the link, not the target)
    println!("[4/7] Rename symlink: rename moves the link itself...");
    let link_old = &format!("{ns}/link_old");
    let link_new = &format!("{ns}/link_new");
    fd::twz_rt_fd_symlink(link_old, "/some/target")
        .map_err(|e| anyhow::anyhow!("symlink: {e:?}"))?;
    fd::twz_rt_fd_rename(link_old, link_new)
        .map_err(|e| anyhow::anyhow!("rename symlink: {e:?}"))?;
    // Old symlink name should be gone.
    assert!(
        fd::twz_rt_fd_readlink(link_old, &mut [0u8; 256]).is_err(),
        "old symlink name should not exist"
    );
    // New name should be a symlink pointing to the original target.
    let mut buf = [0u8; 256];
    let n =
        fd::twz_rt_fd_readlink(link_new, &mut buf).map_err(|e| anyhow::anyhow!("readlink: {e:?}"))?;
    let target = std::str::from_utf8(&buf[..n]).unwrap();
    assert_eq!(target, "/some/target", "symlink target should be preserved");
    println!("      target preserved: {target}");
    println!("      OK");

    // Test 5: Rename a namespace (directory)
    println!("[5/7] Rename namespace: rename a directory entry...");
    let dir_old = &format!("{ns}/dir_old");
    let dir_new = &format!("{ns}/dir_new");
    fd::twz_rt_fd_mkns(dir_old).map_err(|e| anyhow::anyhow!("mkns: {e:?}"))?;
    fd::twz_rt_fd_rename(dir_old, dir_new)
        .map_err(|e| anyhow::anyhow!("rename dir: {e:?}"))?;
    // Creating a file inside the new name should work (the namespace still exists).
    let child = &format!("{dir_new}/child");
    let f4 = fd::twz_rt_fd_open(child, create, rw)
        .map_err(|e| anyhow::anyhow!("create child in renamed dir: {e:?}"))?;
    fd::twz_rt_fd_close(f4);
    println!("      OK");

    // Test 6: Rename across namespaces
    println!("[6/7] Cross-namespace rename: move file between directories...");
    let src_dir = &format!("{ns}/src_dir");
    let dst_dir = &format!("{ns}/dst_dir");
    fd::twz_rt_fd_mkns(src_dir).map_err(|e| anyhow::anyhow!("mkns src: {e:?}"))?;
    fd::twz_rt_fd_mkns(dst_dir).map_err(|e| anyhow::anyhow!("mkns dst: {e:?}"))?;
    let src_file = &format!("{src_dir}/moveme");
    let dst_file = &format!("{dst_dir}/moved");
    let f5 = fd::twz_rt_fd_open(src_file, create, rw)
        .map_err(|e| anyhow::anyhow!("create src: {e:?}"))?;
    fd::twz_rt_fd_close(f5);
    fd::twz_rt_fd_rename(src_file, dst_file)
        .map_err(|e| anyhow::anyhow!("cross-ns rename: {e:?}"))?;
    assert!(
        fd::twz_rt_fd_open(src_file, create_exist, twizzler_rt_abi::bindings::OPEN_FLAG_READ)
            .is_err(),
        "source should not exist after cross-ns rename"
    );
    let f6 = fd::twz_rt_fd_open(dst_file, create_exist, twizzler_rt_abi::bindings::OPEN_FLAG_READ)
        .map_err(|e| anyhow::anyhow!("open dst after cross-ns rename: {e:?}"))?;
    fd::twz_rt_fd_close(f6);
    println!("      OK");

    // Test 7: Rename then rename back (round-trip)
    println!("[7/7] Round-trip rename: rename A->B then B->A...");
    let a = &format!("{ns}/round_a");
    let b = &format!("{ns}/round_b");
    let f7 = fd::twz_rt_fd_open(a, create, rw)
        .map_err(|e| anyhow::anyhow!("create round_a: {e:?}"))?;
    fd::twz_rt_fd_close(f7);
    fd::twz_rt_fd_rename(a, b).map_err(|e| anyhow::anyhow!("rename a->b: {e:?}"))?;
    fd::twz_rt_fd_rename(b, a).map_err(|e| anyhow::anyhow!("rename b->a: {e:?}"))?;
    let f8 = fd::twz_rt_fd_open(a, create_exist, twizzler_rt_abi::bindings::OPEN_FLAG_READ)
        .map_err(|e| anyhow::anyhow!("open round_a after round-trip: {e:?}"))?;
    fd::twz_rt_fd_close(f8);
    assert!(
        fd::twz_rt_fd_open(b, create_exist, twizzler_rt_abi::bindings::OPEN_FLAG_READ).is_err(),
        "round_b should not exist after round-trip rename"
    );
    println!("      OK");

    // Cleanup
    fd::twz_rt_fd_remove(new).ok();
    fd::twz_rt_fd_remove(other).ok();
    fd::twz_rt_fd_remove(link_new).ok();
    fd::twz_rt_fd_remove(child).ok();
    fd::twz_rt_fd_remove(dst_file).ok();
    fd::twz_rt_fd_remove(a).ok();

    Ok(())
}

/// Test UDP sockets and DNS resolution via the net module.
fn test_net() -> Result<()> {
    use smoltcp::wire::IpAddress;
    use std::str::FromStr;

    let map_err = |e: net::NetError| anyhow::anyhow!("{e:?}");

    // Test 1: UDP socket bind
    println!("[1/4] UDP bind: binding socket to ephemeral port...");
    let addr = net::NetAddr {
        ip: IpAddress::from_str("0.0.0.0").unwrap(),
        port: 0,
    };
    let sock = net::NetUdpSocket::bind(addr).map_err(map_err)?;
    let local = sock.local_addr().map_err(map_err)?;
    println!("      bound to port {}", local.port);
    assert!(local.port >= 49152, "expected ephemeral port, got {}", local.port);
    println!("      OK");

    // Test 2: UDP send (fire-and-forget to DNS server port 53)
    println!("[2/4] UDP send: sending packet to 10.0.2.3:53...");
    let remote = net::NetAddr {
        ip: IpAddress::from_str("10.0.2.3").unwrap(),
        port: 53,
    };
    let payload = b"hello-udp-test";
    let n = sock.send_to(payload, remote).map_err(map_err)?;
    assert_eq!(n, payload.len(), "expected to send {} bytes, sent {}", payload.len(), n);
    println!("      sent {} bytes", n);
    println!("      OK");

    // Test 3: Second UDP socket (bind to specific port)
    println!("[3/4] UDP bind: binding socket to specific port 9999...");
    let addr2 = net::NetAddr {
        ip: IpAddress::from_str("10.0.2.15").unwrap(),
        port: 9999,
    };
    let sock2 = net::NetUdpSocket::bind(addr2).map_err(map_err)?;
    let local2 = sock2.local_addr().map_err(map_err)?;
    assert_eq!(local2.port, 9999, "expected port 9999, got {}", local2.port);
    println!("      bound to 10.0.2.15:{}", local2.port);
    println!("      OK");
    drop(sock2);
    drop(sock);

    // Test 4: DNS resolution
    println!("[4/4] DNS resolve: resolving 'google.com' via 10.0.2.3...");
    let addrs = net::resolve_dns("google.com").map_err(map_err)?;
    println!("      resolved to: {:?}", addrs);
    assert!(!addrs.is_empty(), "expected at least one address");
    println!("      OK");

    Ok(())
}
