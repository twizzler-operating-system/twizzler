//! Standalone reproducer for the release-only #GP seen in `naming_core`'s test run.
//!
//! The fault resolves to `hashbrown`'s `FullBucketsIndices::next_impl` inside
//! `RawTable<(String, u32)>::reserve_rehash` -- i.e. libtest's own map of test names, not
//! naming-core code. `kqueue_test` (3 tests) survives and `naming_core` (9 tests) does not,
//! which points at table *growth* rather than anything about naming.
//!
//! This strips libtest and naming out of the picture: it just grows `HashMap<String, u32>`
//! past its initial capacity, forcing repeated rehashes. It also does so on a spawned thread,
//! because `ReferenceRuntime::alloc` picks talc-early vs ferroc based on a per-thread
//! `THREAD_STARTED` flag, and libtest runs every test on its own thread -- so the spawned-thread
//! path is the one libtest actually exercises.
//!
//! Run with: cargo start-qemu --profile release --autostart hmrepro

use std::collections::HashMap;

/// Enough insertions to force several rehashes (hashbrown grows at ~7/8 load factor).
const N: u32 = 512;

/// Build a map the same shape as libtest's: `String` keys, `u32` values, default `RandomState`.
/// Returns the map so the caller decides when it is dropped.
fn grow(label: &str) -> HashMap<String, u32> {
    println!("[{label}] growing HashMap<String, u32> to {N} entries");
    let mut map: HashMap<String, u32> = HashMap::new();
    for i in 0..N {
        // Keys shaped like test names, so string lengths land in the same allocator size
        // classes libtest would hit.
        map.insert(format!("store::tests::multi_put_then_get_{i}"), i);
        // Report around each power-of-two boundary, where the rehash actually happens.
        if i.is_power_of_two() || i == N - 1 {
            println!(
                "[{label}]   inserted {} (len={}, cap={})",
                i,
                map.len(),
                map.capacity()
            );
        }
    }
    println!("[{label}] verifying {N} lookups");
    for i in 0..N {
        let k = format!("store::tests::multi_put_then_get_{i}");
        assert_eq!(map.get(&k), Some(&i), "[{label}] lookup failed for {k}");
    }
    println!("[{label}] iterating {} entries", map.len());
    let sum: u64 = map.values().map(|v| *v as u64).sum();
    let expect: u64 = (0..N as u64).sum();
    assert_eq!(sum, expect, "[{label}] iteration sum mismatch");
    println!("[{label}] OK");
    map
}

/// Replicate `naming_core`'s `store::tests::multi_put_then_get` with no libtest involved.
///
/// Phases 1-3 (plain HashMap growth) pass, and 16 trivial `#[test]`s run through libtest pass
/// too -- so neither our own allocation nor the harness's name map is sufficient. What is left
/// is what the naming tests actually *do*: build a `NameStore`, which creates Twizzler objects,
/// and put/get through it. If the heap corruption originates here, this should fault on its own.
fn namestore_phase() {
    use naming_core::{GetFlags, NameStore};
    use twizzler_rt_abi::object::ObjID;

    println!("[namestore] creating NameStore");
    let store = NameStore::new();
    let session = store.root_session();

    println!("[namestore] 100 puts");
    for i in 0..100u128 {
        session
            .put(format!("k{i}"), ObjID::new(i))
            .unwrap_or_else(|e| panic!("[namestore] put k{i} failed: {e:?}"));
    }

    println!("[namestore] 100 gets");
    for i in 0..100u128 {
        let node = session
            .get(&format!("k{i}"), GetFlags::empty())
            .unwrap_or_else(|e| panic!("[namestore] get k{i} failed: {e:?}"));
        assert_eq!(node.id, ObjID::new(i), "[namestore] wrong id for k{i}");
    }
    println!("[namestore] OK");
}

fn main() {
    println!("== hmrepro start ==");

    // 1. Main thread. This is the path a plain program takes.
    let m1 = grow("main");
    drop(m1);
    println!("-- main-thread map dropped --");

    // 2. Spawned thread. libtest runs each test on its own thread, and the allocator's
    //    THREAD_STARTED branch makes this a genuinely different path from (1).
    let h = std::thread::spawn(|| {
        let m = grow("spawned");
        drop(m);
        println!("-- spawned-thread map dropped --");
    });
    h.join().expect("spawned thread panicked");

    // 3. Cross-thread: build on one thread, grow and drop on another. This is the case where a
    //    table allocated under one allocator regime is reallocated/freed under the other.
    let mut m3 = HashMap::new();
    for i in 0..8u32 {
        m3.insert(format!("seed_{i}"), i);
    }
    println!(
        "-- built seed map on main (len={}, cap={}) --",
        m3.len(),
        m3.capacity()
    );
    let h2 = std::thread::spawn(move || {
        println!("[cross] growing seed map on a different thread");
        for i in 8..N {
            m3.insert(format!("store::tests::multi_put_then_get_{i}"), i);
        }
        println!("[cross] len={}, cap={}", m3.len(), m3.capacity());
        drop(m3);
        println!("[cross] OK");
    });
    h2.join().expect("cross thread panicked");

    // 4. The naming-core workload itself, which is the part hmrepro's earlier phases and the
    //    trivial harness tests both lack.
    namestore_phase();

    // 5. Repeat it, then grow a HashMap afterwards. If the store work corrupts the heap, the map
    //    growth is the thing that trips over it -- that is the order the real failure happens in
    //    (naming work first, libtest's rehash notices second).
    namestore_phase();
    let m4 = grow("after-namestore");
    drop(m4);

    // 6. Concurrent NameStores. Sequential store work passes, so the last structural difference
    //    from the real failure is that libtest runs tests *in parallel* -- naming_core has ~9
    //    NameStores being built and driven on separate threads at once. Note the original bug
    //    reproduces on -smp 1, so interleaving rather than true parallelism is enough.
    const THREADS: usize = 9;
    println!("[concurrent] {THREADS} threads each running the namestore workload");
    let handles: Vec<_> = (0..THREADS)
        .map(|t| {
            std::thread::spawn(move || {
                namestore_phase();
                println!("[concurrent] thread {t} done");
            })
        })
        .collect();
    for (i, h) in handles.into_iter().enumerate() {
        h.join()
            .unwrap_or_else(|_| panic!("[concurrent] thread {i} panicked"));
    }
    println!("[concurrent] OK");

    // 7. And a map growth afterwards, to give any resulting heap damage something to trip over.
    let m5 = grow("after-concurrent");
    drop(m5);

    println!("== hmrepro done: all phases passed ==");
}

/// Phase 2 of the reproducer.
///
/// Running `main` above under `--autostart` passes cleanly, so growing a `HashMap<String, u32>`
/// is not by itself the trigger. These tests exist to isolate the other half: libtest's *own*
/// map of test names. `kqueue_test` (3 tests) survives and `naming_core` (9 tests) faults, so if
/// the harness is what breaks, enough trivially-empty tests here should reproduce it with no
/// naming code, no objects, and no allocation of our own involved.
///
/// Deliberately more than 9, to push the harness's map through the same growth naming_core does.
#[cfg(test)]
mod tests {
    macro_rules! trivial_tests {
        ($($name:ident),* $(,)?) => {
            $(
                #[test]
                fn $name() {
                    assert_eq!(2 + 2, 4);
                }
            )*
        };
    }

    trivial_tests!(
        harness_map_growth_01,
        harness_map_growth_02,
        harness_map_growth_03,
        harness_map_growth_04,
        harness_map_growth_05,
        harness_map_growth_06,
        harness_map_growth_07,
        harness_map_growth_08,
        harness_map_growth_09,
        harness_map_growth_10,
        harness_map_growth_11,
        harness_map_growth_12,
        harness_map_growth_13,
        harness_map_growth_14,
        harness_map_growth_15,
        harness_map_growth_16,
    );
}
