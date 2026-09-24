//! Resident memory hog. Pins anonymous memory until only `--free <MiB>` is left, announces what it
//! holds, and keeps it for the life of the guest. Init starts it (`--memhog-free=<MiB>` on the
//! kernel command line) ahead of the test suite so the demand-paged test binaries have to be
//! replaced rather than simply cached.

use std::time::Duration;

use twizzler_abi::syscall::sys_memory_stats;

const MIB: usize = 1 << 20;
const CHUNK: usize = MIB;

fn usage() -> ! {
    eprintln!("usage: memhog --free <MiB>");
    std::process::exit(2);
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let target = match args.get(1).map(String::as_str) {
        Some("--free") => args
            .get(2)
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or_else(|| usage()),
        _ => usage(),
    } * MIB;

    let total = sys_memory_stats().total_bytes();
    let mut chunks: Vec<Vec<u8>> = Vec::new();
    // Free memory is re-read per chunk: pinning drives the kernel into reclaiming clean pages,
    // which frees memory again, and the hog keeps taking it until the target actually holds.
    loop {
        let free = sys_memory_stats().free_bytes();
        if free <= target || chunks.len() * CHUNK >= total {
            break;
        }
        let mut chunk = Vec::new();
        if chunk.try_reserve_exact(CHUNK).is_err() {
            println!("memhog: allocation failed with {} MiB free", free / MIB);
            break;
        }
        // A non-zero fill: a zero one could be folded into a zeroed allocation and never touch
        // the pages. Written anonymous memory has no backing store, so nothing can reclaim it.
        chunk.resize(CHUNK, 0x4d);
        chunks.push(chunk);
    }
    // Init waits for this line before starting the suite.
    println!(
        "memhog: holding {} MiB, {} MiB free (target {} MiB)",
        chunks.len() * CHUNK / MIB,
        sys_memory_stats().free_bytes() / MIB,
        target / MIB,
    );
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}
