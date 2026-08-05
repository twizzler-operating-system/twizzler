use twizzler_abi::syscall::sys_memory_stats;

/// Chunks are allocated and touched individually so a failure partway through a round still
/// leaves the chunks allocated so far in a checkable state.
const CHUNK_BYTES: usize = 1 << 20; // 1 MiB
/// Bounded per-round target: small and fast under the default (~12 GiB) scenario, but a large
/// fraction of guest memory under `lowmem`'s much smaller `-m` -- which is the point.
const TARGET_BYTES: usize = 256 * (1 << 20); // 256 MiB
const ROUNDS: usize = 3;

fn pattern_byte(chunk_idx: usize, byte_idx: usize) -> u8 {
    (chunk_idx.wrapping_mul(31).wrapping_add(byte_idx)) as u8
}

/// Allocate chunks up to `TARGET_BYTES` (fewer if memory runs out -- see below), touch every byte
/// with a per-chunk pattern, verify every chunk still holds it, then free everything.
fn hog_round() {
    let mut chunks: Vec<Vec<u8>> = Vec::new();
    for i in 0..(TARGET_BYTES / CHUNK_BYTES) {
        let mut chunk = Vec::new();
        // `try_reserve_exact` surfaces allocation failure as a `Result` instead of aborting the
        // process (which is what `Vec::with_capacity`/`resize` do on failure) -- under real memory
        // pressure, running out of room here is an expected outcome, not a bug, so stop growing
        // rather than treating it as a test failure.
        if chunk.try_reserve_exact(CHUNK_BYTES).is_err() {
            break;
        }
        chunk.resize(CHUNK_BYTES, 0);
        for (j, b) in chunk.iter_mut().enumerate() {
            *b = pattern_byte(i, j);
        }
        chunks.push(chunk);
    }

    for (i, chunk) in chunks.iter().enumerate() {
        for (j, b) in chunk.iter().enumerate() {
            assert_eq!(
                *b,
                pattern_byte(i, j),
                "corruption in chunk {i} at byte {j}"
            );
        }
    }
    // `chunks` drops here, freeing everything before the next round starts.
}

#[cfg_attr(test, test)]
fn memhog_rounds() {
    let stats = sys_memory_stats();
    println!(
        "memhog-test: total={} free={} target_per_round={}",
        stats.total_bytes(),
        stats.free_bytes(),
        TARGET_BYTES,
    );
    for round in 0..ROUNDS {
        println!("memhog-test: round {round}");
        hog_round();
    }
}

#[cfg(not(test))]
fn main() {
    memhog_rounds();
    println!("memhog-test: all rounds passed");
}
