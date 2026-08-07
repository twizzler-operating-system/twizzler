use twizzler_abi::syscall::sys_memory_stats;

/// Chunks are allocated and touched individually so a failure partway through a round still
/// leaves the chunks allocated so far in a checkable state.
const CHUNK_BYTES: usize = 1 << 20; // 1 MiB
/// Bounded per-round target: small and fast under the default (~12 GiB) scenario, but a large
/// fraction of guest memory under `lowmem`'s much smaller `-m` -- which is the point.
const TARGET_BYTES: usize = 256 * (1 << 20); // 256 MiB
const ROUNDS: usize = 3;
const PAGE: usize = 4096;

/// `31 * 223 == 1 (mod 256)`, so `pattern_byte` is a bijection in the chunk index for any fixed
/// byte index. That is why a single wrong byte proves nothing about *who* wrote it: every possible
/// value decodes to exactly one live chunk. Only agreement across many bytes is evidence.
const INV31: usize = 223;

fn pattern_byte(chunk_idx: usize, byte_idx: usize) -> u8 {
    (chunk_idx.wrapping_mul(31).wrapping_add(byte_idx)) as u8
}

/// Which chunk would have written `observed` at `byte_idx`. See [INV31] before believing it.
fn alias_chunk(byte_idx: usize, observed: u8) -> usize {
    (observed as usize)
        .wrapping_sub(byte_idx)
        .wrapping_mul(INV31)
        & 0xff
}

/// How many bytes of `range` match each competing story: the value this chunk wrote, zero, and the
/// value chunk `alias` would have written.
fn tally(chunk: &[u8], idx: usize, alias: usize, range: core::ops::Range<usize>) -> (usize, usize, usize) {
    let (mut correct, mut zero, mut aliased) = (0, 0, 0);
    for j in range {
        let b = chunk[j];
        if b == pattern_byte(idx, j) {
            correct += 1;
        }
        if b == 0 {
            zero += 1;
        }
        if b == pattern_byte(alias, j) {
            aliased += 1;
        }
    }
    (correct, zero, aliased)
}

/// Does `[base, base+len)` overlap any *other* chunk's allocation? Damage that is always a suffix
/// of a chunk, starts 32-byte aligned, and can begin mid-page with the rest of that page intact is
/// something writing into the region rather than a page being remapped -- so the question is
/// whether the allocator put someone else there.
fn overlapping(spans: &[(usize, usize)], idx: usize, base: usize, len: usize) -> Option<usize> {
    spans.iter().enumerate().find_map(|(k, &(b, l))| {
        (k != idx && base < b + l && b < base + len).then_some(k)
    })
}

/// The damaged bytes hold `alias`'s pattern at the *same* in-chunk offset. Writing through our
/// pointer and reading back through theirs rules out the two addresses being co-mapped to one
/// frame right now -- but note it cannot tell a copy from a copy-on-write share, since the write
/// breaks the share either way. It also reports whether the alias chunk still holds its own data,
/// which says which of the two is the victim.
fn probe_shared(spans: &[(usize, usize)], idx: usize, alias: usize, off: usize) {
    let (Some(&(ours, our_len)), Some(&(theirs, their_len))) = (spans.get(idx), spans.get(alias))
    else {
        return;
    };
    if off >= our_len || off >= their_len {
        return;
    }
    unsafe {
        let a = (ours + off) as *mut u8;
        let b = (theirs + off) as *mut u8;
        let saved_a = core::ptr::read_volatile(a);
        let before = core::ptr::read_volatile(b);
        core::ptr::write_volatile(a, 0xa5);
        // Read *ours* back too. If a store to this region does not stick, the region is not
        // writable-private memory at all, and "their bytes are here" needs no copy to explain it.
        let ours = core::ptr::read_volatile(a);
        let after = core::ptr::read_volatile(b);
        core::ptr::write_volatile(a, saved_a);
        let verdict = if ours != 0xa5 {
            "OUR OWN STORE WAS DROPPED"
        } else if after == 0xa5 {
            "SHARED PHYSICAL PAGE"
        } else {
            "our store landed, theirs unaffected (copy, or a COW share this write just broke)"
        };
        println!(
            "memhog-test: DAMAGE chunk {idx} probe: alias chunk {alias} at same offset held \
             {before} (its own pattern is {}); wrote 0xa5 through ours -> ours reads {ours}, \
             theirs reads {after} => {verdict}",
            pattern_byte(alias, off),
        );
    }
}

/// Describe the damage in a chunk instead of just naming one byte. The things that separate the
/// candidate causes are the *extent* of the damage, whether the damaged bytes tell one consistent
/// story (all zero, or all some other chunk's pattern), whether re-reading returns the right value
/// (a lost write and a lost mapping look identical at one byte), and whether the damaged addresses
/// belong to another live allocation.
fn report_damage(chunk: &[u8], idx: usize, first_bad: usize, spans: &[(usize, usize)]) {
    let base = chunk.as_ptr() as usize;
    let observed = chunk[first_bad];
    let alias = alias_chunk(first_bad, observed);

    // Re-read through a volatile load: if it now matches, the value was never in memory wrong.
    let reread = unsafe { core::ptr::read_volatile(chunk.as_ptr().add(first_bad)) };

    let page = first_bad / PAGE;
    let (pc, pz, pa) = tally(chunk, idx, alias, page * PAGE..(page + 1) * PAGE);

    let mut bad = 0usize;
    let mut first_bad_page = usize::MAX;
    let mut last_bad_page = 0usize;
    let mut bad_pages = 0usize;
    for p in 0..chunk.len() / PAGE {
        let mut page_bad = 0;
        for j in p * PAGE..(p + 1) * PAGE {
            if chunk[j] != pattern_byte(idx, j) {
                page_bad += 1;
            }
        }
        if page_bad > 0 {
            bad += page_bad;
            bad_pages += 1;
            if first_bad_page == usize::MAX {
                first_bad_page = p;
            }
            last_bad_page = p;
        }
    }

    println!(
        "memhog-test: DAMAGE chunk {idx} base {base:#x} first_bad {first_bad} \
         (page {page}, page_off {}) expected {} observed {observed} reread {reread}{}",
        first_bad % PAGE,
        pattern_byte(idx, first_bad),
        if reread == pattern_byte(idx, first_bad) {
            " TRANSIENT"
        } else {
            ""
        }
    );
    println!(
        "memhog-test: DAMAGE chunk {idx} extent: {bad} bad bytes over {bad_pages} pages, \
         pages {first_bad_page}..={last_bad_page} of {}",
        chunk.len() / PAGE
    );
    println!(
        "memhog-test: DAMAGE chunk {idx} page {page} tally of {PAGE}: correct {pc}, zero {pz}, \
         chunk-{alias} pattern {pa}"
    );

    // Where the damage starts, in absolute terms, and who else claims that address.
    let bad_addr = base + first_bad;
    let overlap = overlapping(spans, idx, bad_addr, chunk.len() - first_bad);
    let alias_base = spans.get(alias).map(|&(b, _)| b).unwrap_or(0);
    println!(
        "memhog-test: DAMAGE chunk {idx} damage starts {bad_addr:#x} (align {}), \
         chunk span {base:#x}..{:#x}, chunk-{alias} base {alias_base:#x} (delta {}), \
         overlaps chunk {}",
        1usize << bad_addr.trailing_zeros().min(20),
        base + chunk.len(),
        (alias_base as isize) - (base as isize),
        overlap
            .map(|k| k as isize)
            .unwrap_or(-1),
    );

    probe_shared(spans, idx, alias, first_bad);

    let dump_at = first_bad & !0xf;
    let mut hex = String::new();
    for j in dump_at..(dump_at + 32).min(chunk.len()) {
        hex.push_str(&format!("{:02x} ", chunk[j]));
    }
    println!("memhog-test: DAMAGE chunk {idx} bytes at {dump_at}: {hex}");
}

/// Allocate chunks up to `TARGET_BYTES` (fewer if memory runs out -- see below), touch every byte
/// with a per-chunk pattern, verify every chunk still holds it, then free everything.
fn hog_round(round: usize) -> bool {
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
        // Verify immediately, before anything else is allocated. This separates "our stores never
        // landed" from "something clobbered us afterwards" -- the two have completely different
        // suspects, and the end-of-round check alone cannot tell them apart.
        if let Some(j) = (0..chunk.len()).find(|&j| chunk[j] != pattern_byte(i, j)) {
            println!(
                "memhog-test: EARLY chunk {i} bad at {j} immediately after writing it \
                 (expected {}, got {})",
                pattern_byte(i, j),
                chunk[j]
            );
        }
        chunks.push(chunk);
    }

    // Verify every chunk rather than stopping at the first bad one: whether the damage is confined
    // to one chunk, one page, or scattered across many is the thing that discriminates between a
    // recycled frame and a lost write, and it costs one pass to find out.
    let spans: Vec<(usize, usize)> = chunks.iter().map(|c| (c.as_ptr() as usize, c.len())).collect();

    let mut damaged = Vec::new();
    for (i, chunk) in chunks.iter().enumerate() {
        if let Some(j) = (0..chunk.len()).find(|&j| chunk[j] != pattern_byte(i, j)) {
            damaged.push((i, j));
        }
    }

    if !damaged.is_empty() {
        println!(
            "memhog-test: round {round}: {} of {} chunks damaged",
            damaged.len(),
            chunks.len()
        );
        for &(i, j) in damaged.iter().take(8) {
            report_damage(&chunks[i], i, j, &spans);
        }
    }
    damaged.is_empty()
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
    let mut ok = true;
    for round in 0..ROUNDS {
        println!("memhog-test: round {round}");
        ok &= hog_round(round);
    }
    assert!(ok, "memory corruption detected (see DAMAGE lines above)");
}

#[cfg(not(test))]
fn main() {
    memhog_rounds();
    println!("memhog-test: all rounds passed");
}
