use std::sync::mpsc;

type TlsBump = unsafe extern "C" fn(u64) -> u64;
type TlsBssSwap = unsafe extern "C" fn(usize, u64) -> u64;

fn check(name: &str, got: u64, expect: u64, failed: &mut bool) {
    if got == expect {
        println!("ok: {name} = {got}");
    } else {
        println!("FAIL: {name} = {got}, expected {expect}");
        *failed = true;
    }
}

fn main() {
    let mut failed = false;

    // A thread that predates the dlopen, so its TLS region predates libllt's module. It calls
    // into the library only after the load, exercising the DTV catch-up path on a thread that
    // is not the one that performed the dlopen.
    let (early_tx, early_rx) = mpsc::channel::<(TlsBump, TlsBssSwap)>();
    let (early_res_tx, early_res_rx) = mpsc::channel::<(u64, u64)>();
    let early_thread = std::thread::spawn(move || {
        let (bump, bss_swap) = early_rx.recv().unwrap();
        let v = unsafe { bump(1) };
        let b = unsafe { bss_swap(3, 33) };
        early_res_tx.send((v, b)).unwrap();
    });

    let lib = unsafe { libloading::Library::new("libllt.so").unwrap() };
    unsafe {
        let add_one: libloading::Symbol<unsafe extern "C" fn(u32) -> u32> =
            lib.get(b"add_one").unwrap();
        let result = add_one(1);
        println!("Result: {}", result);

        let bump = *lib.get::<TlsBump>(b"tls_bump").unwrap();
        let bss_swap = *lib.get::<TlsBssSwap>(b"tls_bss_swap").unwrap();

        // Main thread also predates the module: first access takes the upgrade path.
        check("main tdata init+1", bump(1), 1001, &mut failed);
        check("main tdata +1 again", bump(1), 1002, &mut failed);
        check("main tbss zero-init", bss_swap(3, 42), 0, &mut failed);
        check("main tbss readback", bss_swap(3, 43), 42, &mut failed);

        // The early thread: independent TLS, upgrade path, tbss must read zero there too.
        early_tx.send((bump, bss_swap)).unwrap();
        let (v, b) = early_res_rx.recv().unwrap();
        check("early thread tdata init+1", v, 1001, &mut failed);
        check("early thread tbss zero-init", b, 0, &mut failed);
        early_thread.join().unwrap();

        // A thread spawned after the dlopen gets a region built from the republished
        // template: no upgrade path, fresh values.
        let late = std::thread::spawn(move || {
            let v = bump(5);
            let b = bss_swap(3, 7);
            (v, b)
        });
        let (v, b) = late.join().unwrap();
        check("late thread tdata init+5", v, 1005, &mut failed);
        check("late thread tbss zero-init", b, 0, &mut failed);

        // Other threads' activity must not have touched the main thread's values.
        check("main tdata isolated", bump(0), 1002, &mut failed);
        check("main tbss isolated", bss_swap(3, 0), 43, &mut failed);
    }

    if failed {
        println!("lltest: TLS FAILED");
        std::process::exit(1);
    }
    println!("lltest: TLS PASSED");
}
