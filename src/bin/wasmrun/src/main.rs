//! Wasmtime WebAssembly runtime for Twizzler.
//!
//! Usage:
//!   wasmrun                — run built-in wasmtime demos + WASI tests
//!   wasmrun mandelbrot     — run interactive Mandelbrot (ANSI terminal)
//!   wasmrun mandelbrot-gfx — run graphical Mandelbrot auto-zoom (WASI-GFX)
//!   wasmrun test           — run comprehensive WASI P2 test suite
//!   wasmrun test-net       — test UDP sockets and DNS resolution
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
