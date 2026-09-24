//! Cold-traversal memory latency, in the shape `generate_crate_metadata` actually has.
//!
//! That pass is 69% `tables` + `def-ids`: a walk over every `DefId` pulling query results out of
//! arenas allocated across the whole compilation. It is the compiler's coldest, most scattered
//! read of its own heap, and it is the one pass slower on Twizzler while the compute-bound passes
//! (typeck 0.94x, borrowck 0.97x) are faster. Seven candidate mechanisms have been refuted with
//! system counters; this measures the remaining claim directly, without rustc in the way.
//!
//! Deliberately std-only and allocator-driven: the point is to exercise the same path rustc does
//! (many small heap allocations, then a pointer chase across them), not to measure raw DRAM.
//!
//! Build for the host with `rustc -O` on the same file to get the comparison arm.

use std::time::Instant;

/// Chunk size and count are chosen so the live set is far larger than any cache: rustc's heap for
/// a big crate is a few hundred MiB.
const CHUNK: usize = 64;

struct Args {
    mib: usize,
    laps: usize,
}

fn parse() -> Args {
    let mut a = Args { mib: 512, laps: 2 };
    let v: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < v.len() {
        match v[i].as_str() {
            "--mib" => {
                i += 1;
                a.mib = v.get(i).and_then(|s| s.parse().ok()).unwrap_or(a.mib);
            }
            "--laps" => {
                i += 1;
                a.laps = v.get(i).and_then(|s| s.parse().ok()).unwrap_or(a.laps);
            }
            _ => {}
        }
        i += 1;
    }
    a
}

/// xorshift, so the permutation is identical on both platforms without pulling in a rand crate.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

fn main() {
    // The unittest harness spawns every initrd program with `--bench`. This is a measurement
    // tool, not a test: at its defaults it would allocate 512 MiB and run for seconds in every
    // peer's suite. Opt in explicitly or do nothing.
    if std::env::args().any(|a| a == "--bench") {
        return;
    }
    let args = parse();
    let n = (args.mib * 1024 * 1024) / CHUNK;

    // Phase 1: allocate like an arena -- many small blocks, in order, interleaved with the
    // short-lived churn a real compiler does, so the survivors are not contiguous.
    let t = Instant::now();
    let mut blocks: Vec<Box<[u64; CHUNK / 8]>> = Vec::with_capacity(n);
    let mut churn: Vec<Box<[u64; CHUNK / 8]>> = Vec::new();
    for i in 0..n {
        blocks.push(Box::new([i as u64; CHUNK / 8]));
        if i % 4 == 0 {
            churn.push(Box::new([0u64; CHUNK / 8]));
            if churn.len() > 64 {
                churn.clear();
            }
        }
    }
    let alloc_ms = t.elapsed().as_millis();

    // Second pass over warm memory: drop everything and allocate the same shape again. The pages
    // are back in the allocator's free lists, not the OS's, so this round takes almost no
    // first-touch faults. alloc1 - alloc2 is the fault-path share; alloc2 alone is the
    // allocator's own bookkeeping. Those two want completely different fixes.
    drop(blocks);
    churn.clear();
    let t2 = Instant::now();
    let mut blocks: Vec<Box<[u64; CHUNK / 8]>> = Vec::with_capacity(n);
    for i in 0..n {
        blocks.push(Box::new([i as u64; CHUNK / 8]));
        if i % 4 == 0 {
            churn.push(Box::new([0u64; CHUNK / 8]));
            if churn.len() > 64 {
                churn.clear();
            }
        }
    }
    let alloc2_ms = t2.elapsed().as_millis();

    // Phase 2: a random permutation cycle over the blocks. Each node stores the index of the next,
    // so the chase is strictly serial -- no prefetcher can run ahead, which is the point.
    let mut order: Vec<u32> = (0..n as u32).collect();
    let mut rng = Rng(0x243f_6a88_85a3_08d3);
    for i in (1..n).rev() {
        let j = (rng.next() as usize) % (i + 1);
        order.swap(i, j);
    }
    for w in 0..n {
        let cur = order[w] as usize;
        let nxt = order[(w + 1) % n] as usize;
        blocks[cur][0] = nxt as u64;
    }

    // Phase 3: chase. Read one word per block, following the cycle.
    let start = order[0] as usize;
    let mut acc = 0u64;
    let t = Instant::now();
    for _ in 0..args.laps {
        let mut cur = start;
        for _ in 0..n {
            let nxt = blocks[cur][0] as usize;
            acc = acc.wrapping_add(nxt as u64);
            cur = nxt;
        }
    }
    let chase_ns = t.elapsed().as_nanos() as u64;
    let accesses = (n as u64) * (args.laps as u64);

    // Phase 4: sequential sum over the same blocks, for bandwidth against the same allocation.
    let t = Instant::now();
    let mut sum = 0u64;
    for b in blocks.iter() {
        sum = sum.wrapping_add(b[1]);
    }
    let seq_ns = t.elapsed().as_nanos() as u64;

    // Phase 5: bulk primitives. Metadata encoding moves megabytes and builds large hash maps,
    // while the passes at parity here (typeck 0.94x, borrowck 0.97x) chase pointers instead. If
    // the two platforms' memcpy/memset differ -- glibc's AVX2 versions against compiler_builtins'
    // generic ones -- that is exactly the asymmetry, and nothing else measured has its shape.
    let bulk = 64 * 1024 * 1024;
    let src = vec![7u8; bulk];
    let mut dst = vec![0u8; bulk];
    let t = Instant::now();
    for _ in 0..4 {
        dst.copy_from_slice(&src);
    }
    let cpy_ns = t.elapsed().as_nanos() as u64;
    let t = Instant::now();
    for i in 0..4u8 {
        dst.iter_mut().for_each(|b| *b = i);
    }
    let set_iter_ns = t.elapsed().as_nanos() as u64;
    let t = Instant::now();
    for _ in 0..4 {
        // write_bytes lowers to memset; the loop above does not, so the pair separates the
        // primitive from the codegen around it.
        unsafe { core::ptr::write_bytes(dst.as_mut_ptr(), 0xa5, bulk) };
    }
    let set_ns = t.elapsed().as_nanos() as u64;
    let cpy_gbs = (bulk as f64 * 4.0) / (cpy_ns as f64);
    let set_gbs = (bulk as f64 * 4.0) / (set_ns as f64);
    let seti_gbs = (bulk as f64 * 4.0) / (set_iter_ns as f64);

    // Phase 6: hash-map insert, the def-path-hash-map shape (10% of syn's .rmeta).
    let hn = 1_000_000usize;
    let t = Instant::now();
    let mut map: std::collections::HashMap<u64, u64> = std::collections::HashMap::new();
    let mut r = Rng(0x9e37_79b9_7f4a_7c15);
    for _ in 0..hn {
        let k = r.next();
        map.insert(k, k ^ 1);
    }
    let map_ns = t.elapsed().as_nanos() as u64;

    println!(
        "MEMBULK memcpy_gbs={:.2} memset_gbs={:.2} memset_loop_gbs={:.2} \
         hashmap_ns_per_insert={} maplen={} dstck={}",
        cpy_gbs,
        set_gbs,
        seti_gbs,
        map_ns / hn as u64,
        map.len(),
        dst[0],
    );

    println!(
        "MEMLAT mib={} blocks={} alloc_ms={} alloc2_ms={} chase_ns_per_access={} \
         chase_total_ms={} seq_ns_per_block={} acc={} sum={}",
        args.mib,
        n,
        alloc_ms,
        alloc2_ms,
        chase_ns / accesses,
        chase_ns / 1_000_000,
        seq_ns / n as u64,
        acc,
        sum,
    );
}
