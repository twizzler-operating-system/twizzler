//! elephance, Twizzler arm. See ../../README.md for the experiment protocol and the
//! shared `ELPH` output-line format.

use std::{process::Command, time::Instant};


use clap::{Args, Parser, Subcommand};
use elephance_core::{
    key_at, props_for, query_index, shards_for_entries, DEFAULT_PER_SHARD, DEFAULT_QSEED,
    DEFAULT_SEED,
};
use twizzler_abi::syscall::{
    sys_memory_stats, sys_object_ctrl, sys_thread_stats, MemoryStats, ObjectControlCmd,
};

mod shard;
use shard::Sharded;

#[derive(Parser)]
#[command(name = "elephance", about = "materials-DB hydration/sharing benchmark, Twizzler arm")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create the sharded dataset and register it with the naming service.
    Build(BuildArgs),
    /// Map the dataset and run verified point queries, reporting checkpoint timings.
    Query(QueryArgs),
    /// Spawn N concurrent query processes (experiment 2) and report global deltas.
    Launch(LaunchArgs),
    /// Dump the kernel memory stats relevant to these experiments.
    Stats,
    /// One-boot smoke: persistent build+query, then volatile build + N readers.
    Smoke(SmokeArgs),
    /// Create a file through the naming/fd path and report the object id it got.
    ///
    /// Exercises `NamespaceObject::create_file`, which is the path the persist fix changes --
    /// `ObjectBuilder` (what `build` uses) was never the broken one. Pair with an offline
    /// `debugfs` look for the printed id under `ids/`: present means guest-created objects now
    /// reach the disk store.
    MkFile(MkFileArgs),
    /// Map objects and read one byte per page, forcing their data resident.
    ///
    /// The working alternative to [`Cmd::Preload`], which is advisory: the pager **declines**
    /// prefetch over `MAX_INFLIGHT_PREFETCH` (2) and acks it with no pages, so a whole-object
    /// preload of a 1 GiB store moved 60 pages of 284,809 (see
    /// ../twzsupport/preload-ab-result.md). Touching produces *demand* faults, which are never
    /// declined and already batch ~34 pages each.
    Warm(WarmArgs),
    /// Ask the kernel to fetch objects' pages up front, by id.
    ///
    /// `cache-srv` exposes this too, but both it and the `cache` CLI are commented out of the
    /// root manifest's initrd list, so neither is in a booted image. This is the same
    /// `ObjectControlCmd::Preload` with no service dependency.
    Preload(PreloadArgs),
    /// Run an arbitrary program N times and report what each run costs in page-ins.
    ///
    /// Experiment 2 with a real workload as the witness instead of `query`. The sequential
    /// N=2 case is the sharing claim reduced to one number: if the second process takes
    /// approximately zero faults for pages the first made resident, residency is shared
    /// system-wide rather than per-process. A timing inversion cannot show that -- the second
    /// process could equally be enjoying a warm host page cache underneath the guest, which is
    /// a thing Linux gets too.
    Witness(WitnessArgs),
}

#[derive(Args, Clone)]
struct BuildArgs {
    #[arg(long, default_value_t = 1_000_000)]
    entries: u64,
    #[arg(long, default_value_t = DEFAULT_SEED)]
    seed: u64,
    /// 0 = derive from entry count.
    #[arg(long, default_value_t = 0)]
    shards: u32,
    #[arg(long, default_value = "/data/elephance")]
    name: String,
    /// Volatile objects (experiment 2 isolation); default is persistent.
    #[arg(long = "volatile")]
    volatile_obj: bool,
    /// Skip the post-build durability sync of each shard.
    #[arg(long)]
    no_sync: bool,
}

#[derive(Args, Clone)]
struct QueryArgs {
    #[arg(long)]
    entries: u64,
    #[arg(long, default_value_t = 1_000_000)]
    queries: u64,
    #[arg(long, default_value_t = DEFAULT_SEED)]
    seed: u64,
    #[arg(long, default_value_t = DEFAULT_QSEED)]
    qseed: u64,
    #[arg(long, default_value = "/data/elephance")]
    name: String,
    #[arg(long = "volatile")]
    volatile_obj: bool,
    /// Elapsed-time checkpoints, in completed-query counts.
    #[arg(long, default_value = "1,1000,100000")]
    checkpoints: String,
    #[arg(long, default_value_t = 0)]
    id: u32,
    /// Give each reader id a disjoint query stream instead of a shared one.
    #[arg(long)]
    disjoint: bool,
}

#[derive(Args)]
struct LaunchArgs {
    #[arg(long, default_value_t = 4)]
    n: u32,
    /// Program name to spawn (resolved by the runtime's exec path).
    #[arg(long, default_value = "elephance")]
    exe: String,
    /// After all children exit, poll until kernel_used is stable (or this timeout) and
    /// report settled deltas too. CAUTION: unreaped-thread stacks drain at ~(idle
    /// wakeups)/100 per cpu, so a bounded settle converges with the backlog still
    /// pinned (measured: 24 threads / ~48 MiB after convergence) — settled does NOT
    /// mean stack-free. Off by default until reaping is prompt; the quanta fields
    /// below are the mitigation that works regardless.
    #[arg(long, default_value_t = 0)]
    settle_secs: u64,
    #[command(flatten)]
    query: QueryArgs,
}

#[derive(Args)]
struct MkFileArgs {
    /// Absolute path, e.g. `/twzm/persistcheck`.
    #[arg(long)]
    path: String,
    /// Bytes to write, so the object has content worth persisting.
    #[arg(long, default_value_t = 4096)]
    bytes: usize,
    /// Total file span. If larger than `bytes`, the writes are scattered through a sparse file
    /// instead of written contiguously -- the shape that corrupted llama's image (a KV buffer
    /// truncated to full size, then sparsely dirtied) as against the contiguous write that left
    /// mine `e2fsck`-clean. See ../twzsupport/ext4-block-double-allocation.md.
    #[arg(long, default_value_t = 0)]
    span: usize,
}

#[derive(Args)]
struct WarmArgs {
    /// Object ids, hex, `+`-separated (see [`PreloadArgs::ids`] for why not commas).
    #[arg(long)]
    ids: String,
    /// Bytes between touches. Default 4096 (every page).
    ///
    /// `2097152` touches one byte per 2 MiB region, which is all the kernel needs: a fault into an
    /// *empty* 2 MiB region is widened to a 1024-page aligned request (`obj/data.rs:1031`), so one
    /// touch fetches the region and its successor and assembles the large page. 507 touches then
    /// do what 259,662 do, for the same pages and the same coverage.
    #[arg(long, default_value_t = 4096)]
    stride: u64,
}

#[derive(Args)]
struct PreloadArgs {
    /// Object ids, hex. Separate with `+` (or `,` when invoking this directly).
    ///
    /// `+` exists because `witness --prebuild` is itself comma-separated, so a comma here is eaten
    /// one level up and the second id arrives as a stray positional. Both are accepted so the
    /// standalone spelling stays obvious; only `+` survives being passed through `--prebuild`.
    #[arg(long)]
    ids: String,
}

#[derive(Args)]
struct WitnessArgs {
    /// Program to run, resolved by the runtime's exec path.
    #[arg(long)]
    exe: String,
    #[arg(long, default_value_t = 2)]
    n: u32,
    /// Run all N at once rather than one after another. Sequential answers "does the second
    /// reader pay?"; concurrent answers "does the cost scale with N?". They are different
    /// claims and this flag is the only difference between them.
    #[arg(long, default_value_t = false)]
    concurrent: bool,
    /// Sample the fault counter over this many seconds before the first spawn.
    ///
    /// `faults_global_delta` is system-wide: on an idle guest the background rate is small but
    /// not zero, and a per-process delta near zero is only meaningful against a measured floor
    /// rather than an assumed one.
    #[arg(long, default_value_t = 2)]
    idle_secs: u64,
    /// Program for `--prebuild`, if it is not the one being measured. Defaults to `--exe`.
    #[arg(long, default_value = "")]
    pre_exe: String,
    /// Run the prebuild program once with these arguments before the measured runs, unmeasured.
    ///
    /// Autostart takes a single program, so a self-contained boot needs the dataset built in the
    /// same process tree as the readers. **Comma-separated, not space-separated**: `init` splits
    /// the autostart string on whitespace and does not honour quotes, so a quoted argument arrives
    /// as several argv entries with the quote characters still attached (observed: `"build`,
    /// `--name=x"`). Empty means skip.
    #[arg(long, default_value = "")]
    prebuild: String,
    /// Everything after `--`, passed to every child verbatim.
    #[arg(last = true)]
    child_args: Vec<String>,
}

#[derive(Args)]
struct SmokeArgs {
    #[arg(long, default_value_t = 200_000)]
    entries: u64,
    #[arg(long, default_value_t = 100_000)]
    queries: u64,
    #[arg(long, default_value_t = 2)]
    n: u32,
    #[arg(long, default_value = "elephance")]
    exe: String,
}

fn us(t0: Instant) -> u128 {
    t0.elapsed().as_micros()
}

fn main() {
    match Cli::parse().cmd {
        Cmd::Build(a) => build(a),
        Cmd::Query(a) => query(a),
        Cmd::Launch(a) => launch(a),
        Cmd::Witness(a) => witness(a),
        Cmd::Preload(a) => preload(a),
        Cmd::Warm(a) => warm(a),
        Cmd::MkFile(a) => mkfile(a),
        Cmd::Stats => stats(),
        Cmd::Smoke(a) => smoke(a),
    }
}

fn smoke(a: SmokeArgs) {
    let q = |name: &str, volatile_obj: bool| QueryArgs {
        entries: a.entries,
        queries: a.queries,
        seed: DEFAULT_SEED,
        qseed: DEFAULT_QSEED,
        name: name.to_string(),
        volatile_obj,
        checkpoints: "1,1000".to_string(),
        id: 0,
        disjoint: false,
    };
    // Persistent arm: build, sync, reopen-by-name, verified queries.
    build(BuildArgs {
        entries: a.entries,
        seed: DEFAULT_SEED,
        shards: 0,
        name: "/data/elephance-smoke-p".to_string(),
        volatile_obj: false,
        no_sync: false,
    });
    query(q("/data/elephance-smoke-p", false));
    // Volatile arm: build + N concurrent reader processes.
    build(BuildArgs {
        entries: a.entries,
        seed: DEFAULT_SEED,
        shards: 0,
        name: "/data/elephance-smoke-v".to_string(),
        volatile_obj: true,
        no_sync: true,
    });
    launch(LaunchArgs {
        n: a.n,
        exe: a.exe,
        settle_secs: 0,
        query: q("/data/elephance-smoke-v", true),
    });
    // query() and launch() assert verification and child exit status internally.
    println!("ELPH os=twizzler role=smoke result=OK");
}

fn build(a: BuildArgs) {
    let nshards = if a.shards == 0 {
        shards_for_entries(a.entries, DEFAULT_PER_SHARD)
    } else {
        a.shards
    };
    // Reserve with slack so build never rehashes mid-insert.
    let reserve_per = (a.entries / nshards as u64 + a.entries / nshards as u64 / 8 + 16) as usize;
    let persist = !a.volatile_obj;

    let t0 = Instant::now();
    let mut s = Sharded::create(&a.name, nshards, persist, reserve_per);
    let t_create = us(t0);
    const CHUNK: u64 = 262_144;
    let mut i = 0;
    while i < a.entries {
        let end = (i + CHUNK).min(a.entries);
        s.insert_batch((i..end).map(|j| {
            let key = key_at(a.seed, j);
            (key, props_for(key))
        }));
        i = end;
        eprintln!("elephance build: {i}/{}", a.entries);
    }
    let t_insert = us(t0) - t_create;

    let t_sync = if persist && !a.no_sync {
        let ts = Instant::now();
        for id in s.ids() {
            sys_object_ctrl(id, ObjectControlCmd::Sync, 0, 0).expect("shard sync failed");
        }
        us(ts)
    } else {
        0
    };

    println!(
        "ELPH os=twizzler role=build name={} entries={} seed={:#x} shards={} persist={} \
         t_create_us={} t_insert_us={} t_sync_us={} lens={:?}",
        a.name, a.entries, a.seed, nshards, persist, t_create, t_insert, t_sync, s.lens()
    );
}

fn parse_checkpoints(spec: &str, queries: u64) -> Vec<u64> {
    let mut cps: Vec<u64> = spec
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .filter(|&c| c > 0 && c < queries)
        .collect();
    cps.push(queries);
    cps.sort_unstable();
    cps.dedup();
    cps
}

fn query(a: QueryArgs) {
    let qseed = if a.disjoint { a.qseed ^ (a.id as u64).wrapping_mul(elephance_core::GOLDEN) } else { a.qseed };
    let cps = parse_checkpoints(&a.checkpoints, a.queries);

    let t0 = Instant::now();
    let m0 = sys_memory_stats();
    let s = Sharded::open(&a.name, !a.volatile_obj);
    let t_open = us(t0);

    let mut verified = 0u64;
    let mut cp_times: Vec<(u64, u128)> = Vec::with_capacity(cps.len());
    let mut next_cp = 0;
    for j in 0..a.queries {
        let key = key_at(a.seed, query_index(qseed, j, a.entries));
        let v = s.get(key).expect("key missing from dataset");
        assert!(v.matches(&props_for(key)), "verification failed at query {j}");
        verified += 1;
        if j + 1 == cps[next_cp] {
            cp_times.push((cps[next_cp], us(t0)));
            next_cp += 1;
        }
    }
    let m1 = sys_memory_stats();

    let cp_str: String = cp_times
        .iter()
        .map(|(c, t)| format!(" t_q{c}_us={t}"))
        .collect();
    println!(
        "ELPH os=twizzler role=query id={} name={} entries={} queries={} shards={} disjoint={} \
         t_open_us={}{cp_str} verified={} faults_global_delta={} pagedata_delta_frames={} \
         ktables_delta_frames={} shootdowns_delta={}",
        a.id,
        a.name,
        a.entries,
        a.queries,
        s.nshards(),
        a.disjoint,
        t_open,
        verified,
        m1.page_fault_count - m0.page_fault_count,
        m1.tracker.page_data as i64 - m0.tracker.page_data as i64,
        m1.tracker.kernel_used as i64 - m0.tracker.kernel_used as i64,
        m1.tlb_shootdown_count - m0.tlb_shootdown_count,
    );
}

/// The four counters every phase of this experiment is read through, sampled together so a
/// single reading describes one instant rather than four nearby ones.
struct Counters {
    faults: u64,
    page_data: i64,
    kernel_used: i64,
    shootdowns: u64,
}

fn counters() -> Counters {
    let m: MemoryStats = sys_memory_stats();
    Counters {
        faults: m.page_fault_count as u64,
        page_data: m.tracker.page_data as i64,
        kernel_used: m.tracker.kernel_used as i64,
        shootdowns: m.tlb_shootdown_count as u64,
    }
}

fn delta_str(a: &Counters, b: &Counters) -> String {
    format!(
        "faults_delta={} pagedata_delta_frames={} ktables_delta_frames={} shootdowns_delta={}",
        b.faults.saturating_sub(a.faults),
        b.page_data - a.page_data,
        b.kernel_used - a.kernel_used,
        b.shootdowns.saturating_sub(a.shootdowns),
    )
}

fn mkfile(a: MkFileArgs) {
    use std::io::Write;
    let t = Instant::now();
    // The naming layer will not create an intermediate component, so the parent has to exist
    // before the file can. `AlreadyExists` is the steady state and is not an error. This is also
    // the namespace whose persist state the fix reads back, so creating it here is part of the
    // test rather than setup around it.
    if let Some((parent, _)) = a.path.rsplit_once('/') {
        if !parent.is_empty() {
            let r = twizzler_rt_abi::fd::twz_rt_fd_mkns(parent);
            println!(
                "ELPH os=twizzler role=mkfile_mkns parent={} r={:?}",
                parent,
                r.err()
            );
        }
    }
    let buf = vec![0xABu8; a.bytes];
    let res = (|| -> std::io::Result<()> {
        use std::io::{Seek, SeekFrom};
        let mut f = std::fs::File::create(&a.path)?;
        if a.span > a.bytes {
            // Size first, then dirty scattered pages through the hole -- one 4 KiB page every
            // `stride` bytes. `set_len` grows i_size without allocating, so every write below
            // lands in a hole and forces its own extent.
            f.set_len(a.span as u64)?;
            let pages = (a.bytes / 4096).max(1);
            let stride = (a.span / pages) as u64;
            let page = vec![0xABu8; 4096];
            for i in 0..pages {
                f.seek(SeekFrom::Start(i as u64 * stride))?;
                f.write_all(&page)?;
            }
        } else {
            f.write_all(&buf)?;
        }
        f.sync_all()
    })();
    let id = twizzler_rt_abi::fd::twz_rt_resolve_name(Default::default(), &a.path)
        .map(|id| format!("{:x}", id.raw()))
        .unwrap_or_else(|e| format!("unresolved:{e}"));
    println!(
        "ELPH os=twizzler role=mkfile path={} bytes={} ok={} id={} wall_us={}",
        a.path,
        a.bytes,
        res.is_ok(),
        id,
        us(t)
    );
    if let Err(e) = res {
        println!("ELPH os=twizzler role=mkfile err={e}");
    }
}

fn warm(a: WarmArgs) {
    use twizzler_rt_abi::object::{
        twz_rt_map_object, MapFlags, ObjID, MAX_SIZE, MEXT_SIZED, NULLPAGE_SIZE,
    };
    const PAGE: usize = 0x1000;
    for tok in a.ids.split(['+', ',']).filter(|t| !t.is_empty()) {
        let Ok(raw) = u128::from_str_radix(tok.trim_start_matches("0x"), 16) else {
            println!("ELPH os=twizzler role=warm id={tok} ok=false err=badid");
            continue;
        };
        let t = Instant::now();
        let c0 = counters();
        let handle = match twz_rt_map_object(ObjID::new(raw), MapFlags::READ) {
            Ok(h) => h,
            Err(e) => {
                println!("ELPH os=twizzler role=warm id={raw:x} ok=false err=map:{e}");
                continue;
            }
        };
        // Length from the object itself; touching past it would fault on pages that do not exist.
        let len = handle
            .find_meta_ext(MEXT_SIZED)
            .map(|me| me.value.load(std::sync::atomic::Ordering::SeqCst))
            .unwrap_or(0)
            .min((MAX_SIZE - NULLPAGE_SIZE) as u64);
        // Data starts a null page in, which is the convention `RawFile` reads and writes at
        // (raw_file.rs:382) -- `handle.start()` is the mapping base, not the data.
        let base = unsafe { handle.start().add(NULLPAGE_SIZE) };
        let stride = a.stride.max(1);
        let mut acc: u64 = 0;
        let mut off: u64 = 0;
        let mut touches: u64 = 0;
        while off < len {
            // Volatile so the loop cannot be optimised away: the read *is* the work.
            acc = acc.wrapping_add(unsafe { core::ptr::read_volatile(base.add(off as usize)) } as u64);
            touches += 1;
            off += stride;
        }
        let c1 = counters();
        println!(
            "ELPH os=twizzler role=warm id={:x} ok=true len={} stride={} touched={} covered_pages={} acc={} wall_us={} {}",
            raw,
            len,
            stride,
            touches,
            len.div_ceil(PAGE as u64),
            acc,
            us(t),
            delta_str(&c0, &c1)
        );
    }
}

fn preload(a: PreloadArgs) {
    for tok in a.ids.split(['+', ',']).filter(|t| !t.is_empty()) {
        let Ok(raw) = u128::from_str_radix(tok.trim_start_matches("0x"), 16) else {
            println!("ELPH os=twizzler role=preload id={tok} ok=false err=badid");
            continue;
        };
        let id = twizzler_rt_abi::object::ObjID::new(raw);
        let t = Instant::now();
        let c0 = counters();
        let r = sys_object_ctrl(id, ObjectControlCmd::Preload, 0, 0);
        let c1 = counters();
        // The pages this pulls in are the ones the workload then does not have to. Reported so the
        // cost shows up here rather than vanishing.
        println!(
            "ELPH os=twizzler role=preload id={:x} ok={} wall_us={} {}",
            raw,
            r.is_ok(),
            us(t),
            delta_str(&c0, &c1)
        );
    }
}

fn witness(a: WitnessArgs) {
    // The idle floor first: a per-process delta of "about zero" means nothing without knowing
    // what zero costs on this guest.
    let i0 = counters();
    if a.idle_secs > 0 {
        std::thread::sleep(std::time::Duration::from_secs(a.idle_secs));
    }
    let i1 = counters();
    println!(
        "ELPH os=twizzler role=witness_idle secs={} {}",
        a.idle_secs,
        delta_str(&i0, &i1)
    );

    let spawn = |i: u32| {
        Command::new(&a.exe)
            .args(&a.child_args)
            .spawn()
            .unwrap_or_else(|e| panic!("spawn {} #{} failed: {e}", a.exe, i))
    };

    if !a.prebuild.trim().is_empty() {
        let pre: Vec<&str> = a.prebuild.split(',').filter(|t| !t.is_empty()).collect();
        let tb = Instant::now();
        let pre_exe = if a.pre_exe.is_empty() { &a.exe } else { &a.pre_exe };
        let ok = Command::new(pre_exe)
            .args(&pre)
            .spawn()
            .expect("prebuild spawn failed")
            .wait()
            .expect("prebuild wait failed")
            .success();
        // Reported, not measured: it is setup, and folding it into reader 0's delta would put the
        // whole dataset's first-touch cost on the process that is supposed to show what a *reader*
        // pays.
        println!(
            "ELPH os=twizzler role=witness_prebuild ok={} wall_us={}",
            ok,
            us(tb)
        );
        assert!(ok, "prebuild failed");
    }

    let t0 = Instant::now();
    let c0 = counters();
    let mut failures = 0;

    if a.concurrent {
        // No per-child attribution is possible here -- the counter is global and the children
        // overlap. Reporting a per-child number would be inventing one.
        let children: Vec<_> = (0..a.n).map(spawn).collect();
        let t_spawned = us(t0);
        for mut c in children {
            if !c.wait().expect("wait failed").success() {
                failures += 1;
            }
        }
        let c1 = counters();
        println!(
            "ELPH os=twizzler role=witness exe={} n={} mode=concurrent failures={} \
             t_spawn_all_us={} wall_us={} {}",
            a.exe,
            a.n,
            failures,
            t_spawned,
            us(t0),
            delta_str(&c0, &c1)
        );
    } else {
        for i in 0..a.n {
            let ti = Instant::now();
            let b0 = counters();
            let ok = spawn(i).wait().expect("wait failed").success();
            if !ok {
                failures += 1;
            }
            let b1 = counters();
            println!(
                "ELPH os=twizzler role=witness exe={} i={} mode=sequential ok={} wall_us={} {}",
                a.exe,
                i,
                ok,
                us(ti),
                delta_str(&b0, &b1)
            );
        }
        let c1 = counters();
        println!(
            "ELPH os=twizzler role=witness_total exe={} n={} mode=sequential failures={} \
             wall_us={} {}",
            a.exe,
            a.n,
            failures,
            us(t0),
            delta_str(&c0, &c1)
        );
    }
}

fn launch(a: LaunchArgs) {
    let q = &a.query;
    let mut args = vec![
        "query".to_string(),
        format!("--entries={}", q.entries),
        format!("--queries={}", q.queries),
        format!("--seed={}", q.seed),
        format!("--qseed={}", q.qseed),
        format!("--name={}", q.name),
        format!("--checkpoints={}", q.checkpoints),
    ];
    if q.volatile_obj {
        args.push("--volatile".to_string());
    }
    if q.disjoint {
        args.push("--disjoint".to_string());
    }

    let t0 = Instant::now();
    let m0 = sys_memory_stats();
    let th0 = sys_thread_stats();
    let children: Vec<_> = (0..a.n)
        .map(|i| {
            Command::new(&a.exe)
                .args(&args)
                .arg(format!("--id={i}"))
                .spawn()
                .expect("spawn failed")
        })
        .collect();
    let t_spawned = us(t0);
    let mut failures = 0;
    for mut c in children {
        if !c.wait().expect("wait failed").success() {
            failures += 1;
        }
    }
    let m1 = sys_memory_stats();
    let wall = us(t0);

    // Retained exited-thread stacks move kernel_used in 512-frame (2 MiB) quanta;
    // page tables move in single frames. The div/mod decomposition below carries that
    // signature whether or not the backlog ever drains; the optional settle only
    // becomes a sound instrument once reaping is prompt (see README).
    let (m2, t_settle) = if a.settle_secs > 0 {
        let ts = Instant::now();
        let mut last = m1.tracker.kernel_used;
        let mut stable = 0;
        while ts.elapsed().as_secs() < a.settle_secs && stable < 4 {
            std::thread::sleep(std::time::Duration::from_millis(500));
            let cur = sys_memory_stats().tracker.kernel_used;
            stable = if cur == last { stable + 1 } else { 0 };
            last = cur;
        }
        (sys_memory_stats(), us(ts))
    } else {
        (m1, 0)
    };

    let th1 = sys_thread_stats();
    let ktables_delta = m1.tracker.kernel_used as i64 - m0.tracker.kernel_used as i64;
    println!(
        "ELPH os=twizzler role=launch n={} failures={} t_spawn_all_us={} wall_us={} \
         faults_global_delta={} pagedata_delta_frames={} ktables_delta_frames={} \
         ktables_delta_div512={} ktables_delta_mod512={} \
         exited_backlog_delta={} reaped_delta={} \
         ktables_settled_delta_frames={} pagedata_settled_delta_frames={} t_settle_us={} \
         shootdowns_delta={}",
        a.n,
        failures,
        t_spawned,
        wall,
        m1.page_fault_count - m0.page_fault_count,
        m1.tracker.page_data as i64 - m0.tracker.page_data as i64,
        ktables_delta,
        ktables_delta.div_euclid(512),
        ktables_delta.rem_euclid(512),
        th1.nr_exited_backlog as i64 - th0.nr_exited_backlog as i64,
        th1.nr_reaped as i64 - th0.nr_reaped as i64,
        m2.tracker.kernel_used as i64 - m0.tracker.kernel_used as i64,
        m2.tracker.page_data as i64 - m0.tracker.page_data as i64,
        t_settle,
        m1.tlb_shootdown_count - m0.tlb_shootdown_count,
    );
    assert_eq!(failures, 0, "child query process failed");
}

fn stats() {
    let m: MemoryStats = sys_memory_stats();
    println!(
        "ELPH os=twizzler role=stats total_pages={} faults_global={} shootdowns={} \
         tlb_flushes={} tracker_idle={} tracker_kernel_used={} tracker_page_data={} \
         tracker_total={} allocated={} freed={}",
        m.total_pages,
        m.page_fault_count,
        m.tlb_shootdown_count,
        m.tlb_flush_count,
        m.tracker.idle,
        m.tracker.kernel_used,
        m.tracker.page_data,
        m.tracker.total,
        m.tracker.allocated,
        m.tracker.freed,
    );
}
