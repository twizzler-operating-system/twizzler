use twizzler_abi::syscall::sys_memory_stats;

/// Chunks are allocated and touched individually so a failure partway through a round still
/// leaves the chunks allocated so far in a checkable state.
const CHUNK_BYTES: usize = 1 << 20; // 1 MiB
/// Bounded per-round target: small and fast under the default (~12 GiB) scenario, but a large
/// fraction of guest memory under `lowmem`'s much smaller `-m` -- which is the point.
const TARGET_BYTES: usize = 256 * (1 << 20); // 256 MiB
const ROUNDS: usize = 3;
const PAGE: usize = 4096;

/// Every 16 bytes of a chunk hold one record naming exactly where those bytes belong.
///
/// The previous pattern was `(chunk * 31 + byte_index) as u8`. That is linear in the byte index
/// with slope 1, which makes "chunk B's byte at offset j" and "our own byte from offset j + N"
/// *the same value* for N = 31*(B - A) mod 256. So it could not distinguish a copy from another
/// chunk from a shift of our own data, and every damaged byte decoded to some live chunk by
/// construction. A record that names its own (round, chunk, block) tells them apart directly.
const RECORD: usize = 16;
const TAG: u64 = 0x4d48; // "MH"

/// `[tag:16][round:8][chunk:16][block:24]`. Block index tops out at `CHUNK_BYTES / RECORD`.
fn record_word(round: usize, chunk: usize, block: usize) -> u64 {
    (TAG << 48)
        | ((round as u64 & 0xff) << 40)
        | ((chunk as u64 & 0xffff) << 24)
        | (block as u64 & 0xff_ffff)
}

/// A record is the word followed by its complement, so a half-written or shifted record is not
/// mistaken for a valid one.
fn expected_byte(round: usize, chunk: usize, j: usize) -> u8 {
    let w = record_word(round, chunk, j / RECORD);
    let k = j % RECORD;
    if k < 8 {
        w.to_le_bytes()[k]
    } else {
        (!w).to_le_bytes()[k - 8]
    }
}

fn fill(chunk: &mut [u8], round: usize, idx: usize) {
    for (block, rec) in chunk.chunks_exact_mut(RECORD).enumerate() {
        let w = record_word(round, idx, block);
        rec[0..8].copy_from_slice(&w.to_le_bytes());
        rec[8..16].copy_from_slice(&(!w).to_le_bytes());
    }
}

/// First byte that does not hold what we wrote, or `None`.
fn first_bad(chunk: &[u8], round: usize, idx: usize) -> Option<usize> {
    for (block, rec) in chunk.chunks_exact(RECORD).enumerate() {
        let w = record_word(round, idx, block);
        if rec[0..8] != w.to_le_bytes() || rec[8..16] != (!w).to_le_bytes() {
            let base = block * RECORD;
            return (base..base + RECORD).find(|&j| chunk[j] != expected_byte(round, idx, j));
        }
    }
    None
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Block {
    /// Holds a valid record, naming where these bytes were written.
    Record { round: usize, chunk: usize, block: usize },
    Zero,
    Garbage,
}

fn decode(chunk: &[u8], block: usize) -> Block {
    let rec = &chunk[block * RECORD..block * RECORD + RECORD];
    let w = u64::from_le_bytes(rec[0..8].try_into().unwrap());
    let inv = u64::from_le_bytes(rec[8..16].try_into().unwrap());
    if w >> 48 == TAG && inv == !w {
        Block::Record {
            round: ((w >> 40) & 0xff) as usize,
            chunk: ((w >> 24) & 0xffff) as usize,
            block: (w & 0xff_ffff) as usize,
        }
    } else if rec.iter().all(|&b| b == 0) {
        Block::Zero
    } else {
        Block::Garbage
    }
}

/// Does `[base, base+len)` overlap any *other* chunk's allocation?
fn overlapping(spans: &[(usize, usize)], idx: usize, base: usize, len: usize) -> Option<usize> {
    spans.iter().enumerate().find_map(|(k, &(b, l))| {
        (k != idx && base < b + l && b < base + len).then_some(k)
    })
}

/// Write through our pointer and read back through the source's. Rules out the two addresses being
/// co-mapped to one frame *now*; it cannot tell a copy from a copy-on-write share, since the write
/// breaks the share either way.
fn probe_shared(spans: &[(usize, usize)], idx: usize, src: usize, off: usize) {
    let (Some(&(ours, our_len)), Some(&(theirs, their_len))) = (spans.get(idx), spans.get(src))
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
        let ours_reads = core::ptr::read_volatile(a);
        let after = core::ptr::read_volatile(b);
        core::ptr::write_volatile(a, saved_a);
        let verdict = if ours_reads != 0xa5 {
            "OUR OWN STORE WAS DROPPED"
        } else if after == 0xa5 {
            "SHARED PHYSICAL PAGE"
        } else {
            "our store landed, theirs unaffected (copy, or a COW share this write just broke)"
        };
        println!(
            "memhog-test: DAMAGE chunk {idx} probe: source chunk {src} at same offset held \
             {before}; wrote 0xa5 through ours -> ours reads {ours_reads}, theirs reads {after} \
             => {verdict}"
        );
    }
}

/// Summarize the damaged blocks by what they say about themselves.
struct Survey {
    bad_blocks: usize,
    records: usize,
    zeros: usize,
    garbage: usize,
    /// Distinct (round, chunk) pairs named by damaged records, with a count each.
    sources: Vec<((usize, usize), usize)>,
    /// Distinct `src_block - our_block` deltas, with a count each. A single delta of 0 means the
    /// bytes came from the *same offset* in another chunk; a single nonzero delta means our own
    /// data (or someone's) shifted by that many blocks.
    deltas: Vec<(isize, usize)>,
    first_bad_page: usize,
    last_bad_page: usize,
    bad_pages: usize,
}

fn survey(chunk: &[u8], round: usize, idx: usize, from_block: usize) -> Survey {
    let nblocks = chunk.len() / RECORD;
    let mut s = Survey {
        bad_blocks: 0,
        records: 0,
        zeros: 0,
        garbage: 0,
        sources: Vec::new(),
        deltas: Vec::new(),
        first_bad_page: usize::MAX,
        last_bad_page: 0,
        bad_pages: 0,
    };
    let mut cur_page = usize::MAX;
    for block in from_block..nblocks {
        let w = record_word(round, idx, block);
        let rec = &chunk[block * RECORD..block * RECORD + RECORD];
        if rec[0..8] == w.to_le_bytes() && rec[8..16] == (!w).to_le_bytes() {
            continue;
        }
        s.bad_blocks += 1;
        let page = block * RECORD / PAGE;
        if page != cur_page {
            cur_page = page;
            s.bad_pages += 1;
            if s.first_bad_page == usize::MAX {
                s.first_bad_page = page;
            }
            s.last_bad_page = page;
        }
        match decode(chunk, block) {
            Block::Record { round: r, chunk: c, block: b } => {
                s.records += 1;
                bump(&mut s.sources, (r, c));
                bump(&mut s.deltas, b as isize - block as isize);
            }
            Block::Zero => s.zeros += 1,
            Block::Garbage => s.garbage += 1,
        }
    }
    s
}

fn bump<K: PartialEq>(v: &mut Vec<(K, usize)>, k: K) {
    if let Some(e) = v.iter_mut().find(|e| e.0 == k) {
        e.1 += 1;
    } else if v.len() < 8 {
        v.push((k, 1));
    }
}

/// What the damaged bytes look like as 8-byte words, and whether they repeat.
///
/// The three candidate writers leave different fingerprints, and one hex line is not enough to
/// tell them apart: our own loop storing corrupted register contents repeats with the loop's
/// 256-byte iteration stride; another live allocation writing here looks like whatever that
/// structure is (heap pointers, lengths); memory we simply never wrote looks like neither and
/// carries no trace of our tag.
fn census(chunk: &[u8], round: usize, idx: usize, from: usize) {
    let (mut tagged, mut untagged_tag, mut zero, mut ptrlike, mut chunkbytes) = (0, 0, 0, 0, 0);
    let (mut tag_even, mut tag_odd) = (0usize, 0usize);
    let start = from & !7;
    let mut words = 0usize;
    for (n, w) in chunk[start..].chunks_exact(8).enumerate() {
        let v = u64::from_le_bytes(w.try_into().unwrap());
        words += 1;
        if v >> 48 == TAG {
            tagged += 1;
            if n % 2 == 0 { tag_even += 1 } else { tag_odd += 1 }
        }
        if v >> 48 == !TAG & 0xffff {
            untagged_tag += 1;
            if n % 2 == 0 { tag_even += 1 } else { tag_odd += 1 }
        }
        if v == 0 {
            zero += 1;
        }
        if v == CHUNK_BYTES as u64 {
            chunkbytes += 1;
        }
        // A userspace pointer here looks like 0x0000_00XX_XXXX_XXXX.
        if v >= (1 << 36) && v < (1 << 48) {
            ptrlike += 1;
        }
    }
    println!(
        "memhog-test: DAMAGE r{round} chunk {idx} census of {words} words from {start}: \
         tag(0x{TAG:x}) {tagged}, ~tag {untagged_tag} (even-slot {tag_even}, odd-slot {tag_odd}), \
         zero {zero}, ptr-like {ptrlike}, ==CHUNK_BYTES {chunkbytes}"
    );

    // Self-similarity. Our fill loop stores 256 bytes per iteration, so data it wrote with a
    // clobbered register repeats at 256; a foreign structure repeats at its own stride.
    for period in [16usize, 24, 32, 256, 4096] {
        let end = chunk.len() - period;
        if start >= end {
            continue;
        }
        let (mut same, mut total) = (0usize, 0usize);
        for j in (start..end).step_by(7) {
            total += 1;
            if chunk[j] == chunk[j + period] {
                same += 1;
            }
        }
        if total > 0 && same * 4 >= total {
            println!(
                "memhog-test: DAMAGE r{round} chunk {idx} self-similar at period {period}: \
                 {same}/{total}"
            );
        }
    }

    let mut hex = String::new();
    for j in start..(start + 256).min(chunk.len()) {
        hex.push_str(&format!("{:02x}", chunk[j]));
        if (j - start) % 8 == 7 {
            hex.push(' ');
        }
    }
    println!("memhog-test: DAMAGE r{round} chunk {idx} dump {start}: {hex}");
}

fn report_damage(chunk: &[u8], round: usize, idx: usize, bad: usize, spans: &[(usize, usize)]) {
    let base = chunk.as_ptr() as usize;
    let expected = expected_byte(round, idx, bad);
    let observed = chunk[bad];
    let reread = unsafe { core::ptr::read_volatile(chunk.as_ptr().add(bad)) };

    println!(
        "memhog-test: DAMAGE r{round} chunk {idx} base {base:#x} first_bad {bad} \
         (page {}, page_off {}) expected {expected} observed {observed} reread {reread}{}",
        bad / PAGE,
        bad % PAGE,
        if reread == expected { " TRANSIENT" } else { "" },
    );

    let s = survey(chunk, round, idx, bad / RECORD);
    println!(
        "memhog-test: DAMAGE r{round} chunk {idx} extent: {} bad blocks of {} over {} pages, \
         pages {}..={} of {}; decoded: {} records, {} zero, {} garbage",
        s.bad_blocks,
        chunk.len() / RECORD,
        s.bad_pages,
        s.first_bad_page,
        s.last_bad_page,
        chunk.len() / PAGE,
        s.records,
        s.zeros,
        s.garbage,
    );
    println!(
        "memhog-test: DAMAGE r{round} chunk {idx} sources (round,chunk)xN: {:?}; \
         block deltas (src-ours)xN: {:?}",
        s.sources, s.deltas,
    );

    // The single most informative line: what the damaged bytes say they are, in address terms.
    if let Block::Record { round: r, chunk: c, block: b } = decode(chunk, bad / RECORD) {
        let our_block = bad / RECORD;
        let src_base = spans.get(c).map(|&(x, _)| x).unwrap_or(0);
        let src_addr = src_base + b * RECORD;
        let our_addr = base + our_block * RECORD;
        let story = if c == idx && r == round {
            "OUR OWN DATA, SHIFTED"
        } else if b == our_block {
            "ANOTHER CHUNK, SAME IN-CHUNK OFFSET"
        } else {
            "ANOTHER CHUNK, DIFFERENT OFFSET"
        };
        println!(
            "memhog-test: DAMAGE r{round} chunk {idx} first bad block says r{r} chunk {c} \
             block {b} (ours: r{round} chunk {idx} block {our_block}) => {story}; \
             src addr {src_addr:#x} vs ours {our_addr:#x}, va delta {} ({} pages, {} mod page)",
            src_addr as isize - our_addr as isize,
            (src_addr as isize - our_addr as isize) / PAGE as isize,
            (src_addr as isize - our_addr as isize) % PAGE as isize,
        );
        if c != idx {
            probe_shared(spans, idx, c, bad);
        }
    }

    let bad_addr = base + bad;
    println!(
        "memhog-test: DAMAGE r{round} chunk {idx} damage starts {bad_addr:#x} (align {}), \
         chunk span {base:#x}..{:#x}, overlaps chunk {}",
        1usize << bad_addr.trailing_zeros().min(20),
        base + chunk.len(),
        overlapping(spans, idx, bad_addr, chunk.len() - bad)
            .map(|k| k as isize)
            .unwrap_or(-1),
    );

    census(chunk, round, idx, bad);
}

/// Allocate chunks up to `TARGET_BYTES` (fewer if memory runs out), write a per-chunk record
/// pattern over every byte, verify every chunk still holds it, then free everything.
fn hog_round(round: usize) -> bool {
    let mut chunks: Vec<Vec<u8>> = Vec::new();
    let mut early: Vec<(usize, usize)> = Vec::new();
    for i in 0..(TARGET_BYTES / CHUNK_BYTES) {
        let mut chunk = Vec::new();
        // `try_reserve_exact` surfaces allocation failure as a `Result` instead of aborting the
        // process -- under real memory pressure, running out of room here is expected.
        if chunk.try_reserve_exact(CHUNK_BYTES).is_err() {
            break;
        }
        chunk.resize(CHUNK_BYTES, 0);
        fill(&mut chunk, round, i);
        // Verify immediately, before anything else is allocated. This separates "our stores never
        // landed" from "something clobbered us afterwards".
        if let Some(j) = first_bad(&chunk, round, i) {
            println!("memhog-test: EARLY r{round} chunk {i} bad at {j} immediately after writing it");
            early.push((i, j));
        }
        chunks.push(chunk);
    }

    let spans: Vec<(usize, usize)> =
        chunks.iter().map(|c| (c.as_ptr() as usize, c.len())).collect();

    let mut damaged = Vec::new();
    for (i, chunk) in chunks.iter().enumerate() {
        if let Some(j) = first_bad(chunk, round, i) {
            damaged.push((i, j));
        }
    }

    if !damaged.is_empty() {
        println!(
            "memhog-test: round {round}: {} of {} chunks damaged ({} of them already damaged \
             immediately after being written)",
            damaged.len(),
            chunks.len(),
            early.len(),
        );
        for &(i, j) in damaged.iter().take(8) {
            report_damage(&chunks[i], round, i, j, &spans);
        }
        // Rewrite probe. Writing the same chunk a second time and re-checking separates a
        // one-off loss of our stores from memory that does not hold them at all -- and if it
        // comes back clean and then dirty again, something else is actively writing here.
        for &(i, _) in damaged.iter().take(8) {
            fill(&mut chunks[i], round, i);
            let after = first_bad(&chunks[i], round, i);
            let again = first_bad(&chunks[i], round, i);
            println!(
                "memhog-test: DAMAGE r{round} chunk {i} rewrite probe: after refill {}, \
                 on re-check {}",
                after.map_or("clean".to_string(), |j| format!("bad at {j}")),
                again.map_or("clean".to_string(), |j| format!("bad at {j}")),
            );
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
