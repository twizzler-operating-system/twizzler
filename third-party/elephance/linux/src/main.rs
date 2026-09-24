//! elephance, Linux baseline arms. Two representations of the identical dataset:
//! - flat: a packed record file, hydrated into std::HashMap on open (the "current
//!   practice" arm — pays O(dataset) before the first query).
//! - lmdb: an mmap-backed B-tree, queryable on open (the strong baseline — demand
//!   paged like Twizzler, but per-process page tables and minor faults).
//! See ../../README.md for the protocol and the shared `ELPH` line format.

use std::{
    collections::HashMap,
    fs::File,
    io::{BufReader, BufWriter, Read, Write},
    path::Path,
    time::Instant,
};

use clap::{Args, Parser, Subcommand};
use elephance_core::{
    key_at, props_for, query_index, record_from_bytes, record_to_bytes, FlatHeader, Key, MatProps,
    DEFAULT_QSEED, DEFAULT_SEED, FLAT_HEADER_BYTES, GOLDEN, RECORD_BYTES,
};
use lmdb::Transaction;

#[derive(Parser)]
#[command(name = "elephance-linux", about = "materials-DB benchmark, Linux baseline arms")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Write the flat record file.
    BuildFlat(BuildArgs),
    /// Build the LMDB environment.
    BuildLmdb(BuildArgs),
    /// Hydrate the flat file into a HashMap, then run verified queries.
    QueryFlat(QueryArgs),
    /// Open the LMDB environment and run verified queries (no hydration pass).
    QueryLmdb(QueryArgs),
}

#[derive(Args)]
struct BuildArgs {
    #[arg(long)]
    path: String,
    #[arg(long, default_value_t = 1_000_000)]
    entries: u64,
    #[arg(long, default_value_t = DEFAULT_SEED)]
    seed: u64,
}

#[derive(Args)]
struct QueryArgs {
    #[arg(long)]
    path: String,
    #[arg(long, default_value_t = 1_000_000)]
    queries: u64,
    #[arg(long, default_value_t = DEFAULT_QSEED)]
    qseed: u64,
    #[arg(long, default_value = "1,1000,100000")]
    checkpoints: String,
    #[arg(long, default_value_t = 0)]
    id: u32,
    /// Give each reader id a disjoint query stream instead of a shared one.
    #[arg(long)]
    disjoint: bool,
}

fn us(t0: Instant) -> u128 {
    t0.elapsed().as_micros()
}

/// (minflt, majflt) for this process, from /proc/self/stat fields 10 and 12.
fn faults() -> (u64, u64) {
    let s = std::fs::read_to_string("/proc/self/stat").unwrap();
    // comm may contain spaces; fields are stable after the closing paren.
    let after = &s[s.rfind(')').unwrap() + 2..];
    let f: Vec<&str> = after.split_whitespace().collect();
    (f[7].parse().unwrap(), f[9].parse().unwrap())
}

/// A field from /proc/self/status, in kB.
fn status_kb(field: &str) -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap();
    s.lines()
        .find(|l| l.starts_with(field))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

/// Global page-table memory in kB, from /proc/meminfo (the experiment-2 metric).
fn global_pagetables_kb() -> u64 {
    let s = std::fs::read_to_string("/proc/meminfo").unwrap();
    s.lines()
        .find(|l| l.starts_with("PageTables:"))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
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

fn main() {
    match Cli::parse().cmd {
        Cmd::BuildFlat(a) => build_flat(&a),
        Cmd::BuildLmdb(a) => build_lmdb(&a),
        Cmd::QueryFlat(a) => run_query(&a, || FlatArm::open(&a.path)),
        Cmd::QueryLmdb(a) => run_query(&a, || LmdbArm::open(&a.path)),
    }
}

fn build_flat(a: &BuildArgs) {
    let t0 = Instant::now();
    let mut w = BufWriter::new(File::create(&a.path).unwrap());
    w.write_all(&FlatHeader { entries: a.entries, seed: a.seed }.to_bytes())
        .unwrap();
    for i in 0..a.entries {
        let key = key_at(a.seed, i);
        w.write_all(&record_to_bytes(key, &props_for(key))).unwrap();
    }
    w.into_inner().unwrap().sync_all().unwrap();
    println!(
        "ELPH os=linux role=build arm=flat path={} entries={} seed={:#x} t_build_us={}",
        a.path,
        a.entries,
        a.seed,
        us(t0)
    );
}

fn build_lmdb(a: &BuildArgs) {
    let t0 = Instant::now();
    std::fs::create_dir_all(&a.path).unwrap();
    // Records are 80B; ~3x covers B-tree overhead.
    let map_size = (a.entries as usize * RECORD_BYTES * 3).max(1 << 24);
    let env = lmdb::Environment::new()
        .set_map_size(map_size)
        .open(Path::new(&a.path))
        .unwrap();
    let db = env.open_db(None).unwrap();
    let mut i = 0u64;
    while i < a.entries {
        let mut txn = env.begin_rw_txn().unwrap();
        let batch_end = (i + 1_000_000).min(a.entries);
        while i < batch_end {
            let key = key_at(a.seed, i);
            txn.put(
                db,
                &key.as_u128().to_be_bytes(),
                &props_for(key).to_bytes(),
                lmdb::WriteFlags::empty(),
            )
            .unwrap();
            i += 1;
        }
        txn.commit().unwrap();
    }
    env.sync(true).unwrap();
    println!(
        "ELPH os=linux role=build arm=lmdb path={} entries={} seed={:#x} map_size={} t_build_us={}",
        a.path,
        a.entries,
        a.seed,
        map_size,
        us(t0)
    );
}

trait QueryTarget {
    const ARM: &'static str;
    fn entries(&self) -> u64;
    fn seed(&self) -> u64;
    /// A verified point-lookup closure. Per-query setup (e.g. LMDB read txns) is
    /// hoisted here so the arms compare lookup cost, not handle churn.
    fn reader(&self) -> Box<dyn Fn(Key) -> bool + '_>;
}

/// Hydration arm: reads and inserts every record before the first query can run.
struct FlatArm {
    map: HashMap<Key, MatProps>,
    header: FlatHeader,
}

impl FlatArm {
    fn open(path: &str) -> Self {
        let mut r = BufReader::with_capacity(1 << 20, File::open(path).unwrap());
        let mut hb = [0u8; FLAT_HEADER_BYTES];
        r.read_exact(&mut hb).unwrap();
        let header = FlatHeader::from_bytes(&hb).unwrap();
        let mut map = HashMap::with_capacity(header.entries as usize);
        let mut rb = [0u8; RECORD_BYTES];
        for _ in 0..header.entries {
            r.read_exact(&mut rb).unwrap();
            let (k, p) = record_from_bytes(&rb);
            map.insert(k, p);
        }
        FlatArm { map, header }
    }
}

impl QueryTarget for FlatArm {
    const ARM: &'static str = "flat";
    fn entries(&self) -> u64 {
        self.header.entries
    }
    fn seed(&self) -> u64 {
        self.header.seed
    }
    fn reader(&self) -> Box<dyn Fn(Key) -> bool + '_> {
        Box::new(|key| self.map.get(&key) == Some(&props_for(key)))
    }
}

/// Strong baseline: mmap-backed, demand-paged, queryable on open.
struct LmdbArm {
    env: lmdb::Environment,
    db: lmdb::Database,
    header: FlatHeader,
}

impl LmdbArm {
    fn open(path: &str) -> Self {
        let data_len = std::fs::metadata(Path::new(path).join("data.mdb"))
            .map(|m| m.len() as usize)
            .unwrap_or(0);
        let env = lmdb::Environment::new()
            .set_flags(lmdb::EnvironmentFlags::READ_ONLY)
            .set_map_size(data_len + (1 << 24))
            .open(Path::new(path))
            .unwrap();
        let db = env.open_db(None).unwrap();
        // Entry count from the env stat (main DB); the seed is not stored, so pass
        // ELEPHANCE_SEED when building with a non-default one.
        let entries = env.stat().unwrap().entries() as u64;
        let seed = std::env::var("ELEPHANCE_SEED")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_SEED);
        LmdbArm { env, db, header: FlatHeader { entries, seed } }
    }
}

impl QueryTarget for LmdbArm {
    const ARM: &'static str = "lmdb";
    fn entries(&self) -> u64 {
        self.header.entries
    }
    fn seed(&self) -> u64 {
        self.header.seed
    }
    fn reader(&self) -> Box<dyn Fn(Key) -> bool + '_> {
        let txn = self.env.begin_ro_txn().unwrap();
        let db = self.db;
        Box::new(move |key| match txn.get(db, &key.as_u128().to_be_bytes()) {
            Ok(v) => v == props_for(key).to_bytes(),
            Err(_) => false,
        })
    }
}

fn run_query<T: QueryTarget>(a: &QueryArgs, open: impl FnOnce() -> T) {
    let t0 = Instant::now();
    let f0 = faults();
    let target = open();
    let t_open = us(t0);
    let qseed = if a.disjoint { a.qseed ^ (a.id as u64).wrapping_mul(GOLDEN) } else { a.qseed };
    let cps = parse_checkpoints(&a.checkpoints, a.queries);
    let mut verified = 0u64;
    let mut cp_times: Vec<(u64, u128)> = Vec::with_capacity(cps.len());
    let mut next_cp = 0;
    let read = target.reader();
    for j in 0..a.queries {
        let key = key_at(target.seed(), query_index(qseed, j, target.entries()));
        assert!(read(key), "verification failed at query {j}");
        verified += 1;
        if j + 1 == cps[next_cp] {
            cp_times.push((cps[next_cp], us(t0)));
            next_cp += 1;
        }
    }
    let (minflt1, majflt1) = faults();
    let cp_str: String = cp_times
        .iter()
        .map(|(c, t)| format!(" t_q{c}_us={t}"))
        .collect();
    println!(
        "ELPH os=linux role=query arm={} id={} path={} entries={} queries={} disjoint={} \
         t_open_us={t_open}{cp_str} verified={verified} minflt_delta={} majflt_delta={} \
         vmpte_kb={} vmhwm_kb={} global_pagetables_kb={}",
        T::ARM,
        a.id,
        a.path,
        target.entries(),
        a.queries,
        a.disjoint,
        minflt1 - f0.0,
        majflt1 - f0.1,
        status_kb("VmPTE:"),
        status_kb("VmHWM:"),
        global_pagetables_kb(),
    );
}
