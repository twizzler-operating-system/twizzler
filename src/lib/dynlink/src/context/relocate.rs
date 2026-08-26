use std::{
    collections::{HashMap, HashSet},
    mem::size_of,
    sync::Arc,
    time::Instant,
};

use twizzler_abi::object::ObjID;

use elf::{
    abi::{
        DF_TEXTREL, DT_FLAGS, DT_FLAGS_1, DT_JMPREL, DT_PLTGOT, DT_PLTREL, DT_PLTRELSZ, DT_REL,
        DT_RELA, DT_RELAENT, DT_RELASZ, DT_RELENT, DT_RELSZ,
    },
    endian::NativeEndian,
    parse::{ParseAt, ParsingIterator},
    relocation::{Rel, Rela},
    string_table::StringTable,
    symbol::SymbolTable,
};
use petgraph::graph::NodeIndex;
use smallstr::SmallString;
use tracing::{debug, error, trace};

use super::{Context, Library};
use crate::{
    compartment::CompartmentId,
    library::{LibraryId, RelocState},
    symbol::RelocatedSymbol,
    DynlinkError, DynlinkErrorKind, Vec, SMALL_STRING_SIZE, SMALL_VEC_SIZE,
};

/// Cross-load memoization of symbol resolutions.
///
/// A resolution *decision* -- "name X binds to library L at symbol S" -- depends on the relocating
/// library's deps search list (its members, order, and compartment relationship), the members'
/// symbol tables (fixed per source ELF object), and the gate rules. It does not depend on the
/// per-instance base addresses, which only enter when the decision becomes a written value. So the
/// search can run once per (library source object, deps shape) and be replayed as a map probe on
/// every later load -- which is every spawn after the first, since libstd/libc/libtwz_rt keep the
/// same deps list in every compartment.
///
/// Only resolutions that bound to a *same-compartment* member of the deps list are memoized.
/// Anything that resolved cross-compartment or through the global search -- the weak gate imports,
/// notably -- always runs the live lookup, so memoization never bypasses the gate-permission
/// checks and never depends on context-wide state the fingerprint cannot see.
///
/// Off until the VERIFY-armed validation sweep passes; flip with [`RELOC_MEMO_VERIFY`] for that
/// run, then ship true.
pub(crate) const RELOC_MEMO: bool = true;

/// Verify mode: every replayed resolution also runs the live lookup and the two are compared
/// (defining library and raw value). Disagreements are counted per `relocate_all` and reported
/// through the `reloc_all` debug line as `memo_bad`. Must read 0; run a full `--tests` sweep with
/// this on before trusting a change to the memo or to the lookup rules it shadows.
pub(crate) const RELOC_MEMO_VERIFY: bool = true;

/// One library source object's memoized resolutions; see [`RELOC_MEMO`].
pub(crate) struct LibReplayMemo {
    /// (source object, same-compartment-as-relocatee) for each deps-list entry, in BFS order.
    /// Compared wholesale before replay; any mismatch (different deps, an LD_PRELOAD shadow, a
    /// cross-compartment substitution) falls back to live lookup for the whole library.
    fingerprint: std::vec::Vec<(ObjID, bool)>,
    /// name -> (deps-list ordinal of the defining library, its ELF symbol). Replay rebuilds the
    /// exact `RelocatedSymbol` a live lookup would have returned.
    syms: HashMap<String, (u32, elf::symbol::Symbol)>,
}

#[derive(Default)]
pub(crate) struct RelocCache<'a> {
    syms: HashMap<CompartmentId, HashMap<String, RelocatedSymbol<'a>>>,
    /// Resolutions served from the cache (cheap).
    pub(crate) hits: usize,
    /// Resolutions that fell through to a full lookup_symbol() search (expensive: walks the
    /// dependency list, then the whole graph on miss). Reported so the per-lookup cost can be
    /// derived from the relocation time.
    pub(crate) misses: usize,
    /// Time spent inside lookup_symbol() on misses only. Cache hits are not timed: they are cheap,
    /// and clocking every relocation would cost more than the thing being measured (there are
    /// thousands of relocations but only hundreds of misses).
    pub(crate) resolve_time: std::time::Duration,
    /// Replay source for the library currently being relocated (set per `relocate_single`).
    memo: Option<Arc<LibReplayMemo>>,
    /// Recording target for the library currently being relocated, when it has no valid memo yet.
    record: Option<(ObjID, LibReplayMemo)>,
    /// Resolutions served by replay this `relocate_all`.
    pub(crate) memo_hits: usize,
    /// Verify-mode disagreements; must stay 0.
    pub(crate) memo_bad: usize,
}

impl<'a> RelocCache<'a> {
    /// Replay a memoized resolution for `name`, if the current library has a validated memo
    /// containing it. Runs before any cache or bloom probe.
    pub(crate) fn memo_probe<'b>(
        &mut self,
        ctx: &'b Context,
        name: &str,
        deps_list: &[NodeIndex],
    ) -> Option<RelocatedSymbol<'b>> {
        let m = self.memo.as_ref()?;
        let (ord, sym) = m.syms.get(name)?;
        // The fingerprint check in relocate_single proved every deps entry loaded and matching.
        let dep = ctx.library_deps[*deps_list.get(*ord as usize)?].loaded()?;
        self.memo_hits += 1;
        Some(RelocatedSymbol::new(sym.clone(), dep))
    }

    /// Record a live resolution into the current library's pending memo, when it qualifies:
    /// a real symbol (not weak-zero), defined by a same-compartment member of the deps list.
    pub(crate) fn memo_record(
        &mut self,
        lib: &Library,
        name: &str,
        sym: &RelocatedSymbol<'_>,
        deps_list: &[NodeIndex],
    ) {
        let Some((_, rec)) = self.record.as_mut() else {
            return;
        };
        let Some(elf_sym) = sym.elf_sym() else {
            return;
        };
        if !sym.lib.in_same_compartment_as(lib) {
            return;
        }
        let Some(ord) = deps_list.iter().position(|n| *n == sym.lib.idx) else {
            return;
        };
        rec.syms
            .insert(name.to_string(), (ord as u32, elf_sym.clone()));
    }
}

impl Context {
    /// (source object, same-compartment) per deps-list entry, or None if any entry is unloaded
    /// (in which case neither replay nor recording is safe).
    fn deps_fingerprint(
        &self,
        lib: &Library,
        deps_list: &[NodeIndex],
    ) -> Option<std::vec::Vec<(ObjID, bool)>> {
        deps_list
            .iter()
            .map(|n| {
                self.library_deps[*n]
                    .loaded()
                    .map(|dep| (dep.full_obj.id(), dep.in_same_compartment_as(lib)))
            })
            .collect()
    }
}

impl<'a> RelocCache<'a> {
    pub fn find(&mut self, name: &str, from: CompartmentId) -> Option<&RelocatedSymbol<'a>> {
        // One probe of the per-compartment map, counters from its result -- this used to hash the
        // same name three times (contains_key, entry, get) per relocation.
        let found = self.syms.entry(from).or_default().get(name);
        if found.is_some() {
            self.hits += 1;
        } else {
            self.misses += 1;
        }
        found
    }

    pub fn insert(&mut self, name: &str, from: CompartmentId, sym: RelocatedSymbol<'a>) {
        let entry = self.syms.entry(from).or_default();
        entry.insert(name.to_string(), sym);
    }
}

// A relocation is either a REL type or a RELA type. The only difference is that
// the RELA type contains an addend (used in the reloc calculations below).
#[derive(Debug)]
pub(crate) enum EitherRel {
    Rel(Rel),
    Rela(Rela),
}

impl EitherRel {
    pub fn r_type(&self) -> u32 {
        match self {
            EitherRel::Rel(r) => r.r_type,
            EitherRel::Rela(r) => r.r_type,
        }
    }

    pub fn addend(&self, target: *mut u64) -> i64 {
        match self {
            EitherRel::Rel(_) => unsafe { target.read() as i64 },
            EitherRel::Rela(r) => r.r_addend,
        }
    }

    pub fn offset(&self) -> u64 {
        match self {
            EitherRel::Rel(r) => r.r_offset,
            EitherRel::Rela(r) => r.r_offset,
        }
    }

    pub fn sym(&self) -> u32 {
        match self {
            EitherRel::Rel(r) => r.r_sym,
            EitherRel::Rela(r) => r.r_sym,
        }
    }
}

impl Context {
    pub(crate) fn get_parsing_iter<P: ParseAt>(
        &self,
        start: *const u8,
        ent: usize,
        sz: usize,
    ) -> Option<ParsingIterator<'_, NativeEndian, P>> {
        P::validate_entsize(elf::file::Class::ELF64, ent).ok()?;
        let iter = ParsingIterator::new(NativeEndian, elf::file::Class::ELF64, unsafe {
            core::slice::from_raw_parts(start, sz)
        });
        Some(iter)
    }

    #[allow(clippy::too_many_arguments)]
    fn process_rels(
        &self,
        lib: &Library,
        start: *const u8,
        ent: usize,
        sz: usize,
        name: &str,
        strings: &StringTable,
        syms: &SymbolTable<NativeEndian>,
        deps_list: &[NodeIndex],
        reloc_cache: &mut RelocCache<'_>,
    ) -> Result<(), DynlinkError> {
        debug!(
            "{}: processing {} relocations (num = {})",
            lib,
            name,
            sz / ent
        );
        // Try to parse the table as REL or RELA, according to ent size. If get_parsing_iter
        // succeeds for a given relocation type, that's the correct one.
        if let Some(rels) = self.get_parsing_iter(start, ent, sz) {
            DynlinkError::collect(
                DynlinkErrorKind::RelocationSectionFail {
                    secname: "REL".into(),
                    library: lib.name.as_str().into(),
                },
                rels.map(|rel| {
                    self.do_reloc(
                        lib,
                        EitherRel::Rel(rel),
                        strings,
                        syms,
                        deps_list,
                        reloc_cache,
                    )
                }),
            )?;
            Ok(())
        } else if let Some(relas) = self.get_parsing_iter(start, ent, sz) {
            DynlinkError::collect(
                DynlinkErrorKind::RelocationSectionFail {
                    secname: "RELA".into(),
                    library: lib.name.as_str().into(),
                },
                relas.map(|rela| {
                    self.do_reloc(
                        lib,
                        EitherRel::Rela(rela),
                        strings,
                        syms,
                        deps_list,
                        reloc_cache,
                    )
                }),
            )?;
            Ok(())
        } else {
            let info = format!("reloc '{}' with entsz {}, size {}", name, ent, sz);
            Err(DynlinkErrorKind::UnsupportedReloc {
                library: lib.name.as_str().into(),
                reloc: info.into(),
            }
            .into())
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn process_relr(
        &self,
        lib: &Library,
        start: *const u8,
        ent: usize,
        sz: usize,
    ) -> Result<(), DynlinkError> {
        tracing::debug!(
            "{}: processing RELR relocations (num = {}) at {:p}",
            lib,
            sz / ent,
            start
        );
        // These are different, they indicate simple base additions based
        // on a compressed format.

        let relr_slice: &[usize] =
            unsafe { std::slice::from_raw_parts(start as *const usize, sz / ent) };

        let base = lib.base_addr();
        let mut target = 0;

        let reloc_at = |target: usize, base: usize| {
            tracing::trace!("processing relr at {:x} += {:x}", target, base);
            let ptr = unsafe { (target as *mut usize).as_mut().unwrap() };
            (*ptr) += base;
            tracing::trace!("new value is {:x}", *ptr);
        };

        let mut j = 0;
        for entry in relr_slice {
            tracing::trace!("RELR: found [{}] {:x}", j, *entry);
            if *entry & 1 != 0 {
                if target == 0 {
                    return Err(DynlinkError {
                        kind: DynlinkErrorKind::Unknown,
                        related: Default::default(),
                    });
                }
                // LSB set -- its a bitmap
                for i in 0..(size_of::<usize>() * 8 - 1) {
                    if (entry >> (i + 1)) & 1 != 0 {
                        reloc_at(target + size_of::<usize>() * i, base);
                    }
                }
                target += size_of::<usize>() * (8 * size_of::<usize>() - 1);
            } else {
                // sets the address
                reloc_at(base + *entry, base);
                target = base + entry + size_of::<usize>();
            }
            j += 1;
        }
        Ok(())
    }

    pub(crate) fn relocate_single(
        &self,
        lib_id: LibraryId,
        reloc_cache: &mut RelocCache<'_>,
    ) -> Result<(), DynlinkError> {
        let _start_1 = Instant::now();
        let (_hits_0, _misses_0) = (reloc_cache.hits, reloc_cache.misses);
        let _resolve_0 = reloc_cache.resolve_time;
        let lib = self.get_library(lib_id)?;
        debug!("{}: relocating library", lib);
        let common = lib.get_elf_common()?;
        let dynamic = common
            .dynamic
            .as_ref()
            .ok_or_else(|| DynlinkErrorKind::MissingSection {
                name: "dynamic".into(),
            })?;

        // Helper to lookup a single entry for a relocated pointer in the dynamic table.
        let find_dyn_entry = |tag| {
            dynamic
                .iter()
                .find(|d| d.d_tag == tag)
                .map(|d| lib.laddr(d.d_ptr()))
        };

        // Helper to lookup a single value in the dynamic table.
        let find_dyn_value = |tag| dynamic.iter().find(|d| d.d_tag == tag).map(|d| d.d_val());

        // Many of the relocation tables are described in a similar way -- start, entry size, and
        // table size (in bytes).
        let find_dyn_rels = |tag, ent, sz| {
            let rel = find_dyn_entry(tag);
            let relent = find_dyn_value(ent);
            let relsz = find_dyn_value(sz);
            if let (Some(rel), Some(relent), Some(relsz)) = (rel, relent, relsz) {
                Some((rel, relent, relsz))
            } else {
                None
            }
        };

        let flags = find_dyn_value(DT_FLAGS);
        let flags_1 = find_dyn_value(DT_FLAGS_1);
        if let Some(flags) = flags {
            if flags as i64 & DF_TEXTREL != 0 {
                error!("{}: relocations within text not supported", lib);
                return Err(DynlinkErrorKind::UnsupportedReloc {
                    library: lib.name.as_str().into(),
                    // TODO
                    reloc: "DF_TEXTREL".into(),
                }
                .into());
            }
        }
        debug!("{}: relocation flags: {:?} {:?}", lib, flags, flags_1);

        // these aren't in elf v0.8.0
        const DT_RELR: i64 = 0x24;
        const DT_RELRENT: i64 = 0x25;
        const DT_RELRSZ: i64 = 0x23;
        // Lookup all the tables
        let rels = find_dyn_rels(DT_REL, DT_RELENT, DT_RELSZ);
        let relas = find_dyn_rels(DT_RELA, DT_RELAENT, DT_RELASZ);
        let relr = find_dyn_rels(DT_RELR, DT_RELRENT, DT_RELRSZ);
        let jmprels = find_dyn_rels(DT_JMPREL, DT_PLTREL, DT_PLTRELSZ);
        let _pltgot: Option<*const u8> = find_dyn_entry(DT_PLTGOT);

        let dynsyms = common
            .dynsyms
            .as_ref()
            .ok_or_else(|| DynlinkErrorKind::MissingSection {
                name: "dynsyms".into(),
            })?;
        let dynsyms_str =
            common
                .dynsyms_strs
                .as_ref()
                .ok_or_else(|| DynlinkErrorKind::MissingSection {
                    name: "dynsyms_strs".into(),
                })?;

        let deps_list = self.build_deps_search_list(lib.id());

        // Arm the resolution memo for this library: replay if a memo exists and its deps
        // fingerprint matches, otherwise record into a fresh one. See [`RELOC_MEMO`].
        reloc_cache.memo = None;
        reloc_cache.record = None;
        if RELOC_MEMO {
            if let Some(fp) = self.deps_fingerprint(lib, deps_list.as_slice()) {
                let src = lib.full_obj.id();
                if let Ok(memos) = self.reloc_memo.lock() {
                    if let Some(m) = memos.get(&src) {
                        if m.fingerprint == fp {
                            reloc_cache.memo = Some(m.clone());
                        }
                    }
                }
                if reloc_cache.memo.is_none() {
                    reloc_cache.record = Some((
                        src,
                        LibReplayMemo {
                            fingerprint: fp,
                            syms: HashMap::new(),
                        },
                    ));
                }
            }
        }
        let _start_2 = Instant::now();

        // Process relocations

        // RELR carries no symbol references at all, so this phase is pure apply: a linear
        // `*ptr += base` sweep over the relocated words. It is the clean measure of what
        // relocation costs once symbol resolution is removed -- including the COW faults taken
        // on the data object the first time each page is written.
        let _relr_start = Instant::now();
        if let Some((rel, ent, sz)) = relr {
            self.process_relr(lib, rel, ent as usize, sz as usize)?;
        }
        let _relr_time = _relr_start.elapsed();
        let _rels_start = Instant::now();

        if let Some((rela, ent, sz)) = relas {
            self.process_rels(
                lib,
                rela,
                ent as usize,
                sz as usize,
                "RELA",
                &dynsyms_str,
                &dynsyms,
                deps_list.as_slice(),
                reloc_cache,
            )?;
        }

        if let Some((rel, ent, sz)) = rels {
            self.process_rels(
                lib,
                rel,
                ent as usize,
                sz as usize,
                "REL",
                &dynsyms_str,
                &dynsyms,
                deps_list.as_slice(),
                reloc_cache,
            )?;
        }

        // This one is a little special in that instead of an entry size, we are given a relocation
        // type.
        if let Some((rel, kind, sz)) = jmprels {
            let ent = match kind as i64 {
                DT_REL => 2,  // 2 usize long, according to ELF
                DT_RELA => 3, // one extra usize for the addend
                _ => {
                    error!("failed to relocate {}: unknown PLTREL type", lib);
                    return Err(DynlinkErrorKind::UnsupportedReloc {
                        library: lib.name.as_str().into(),
                        reloc: "unknown PTREL type".into(),
                    }
                    .into());
                }
            } * size_of::<usize>();
            self.process_rels(
                lib,
                rel,
                ent,
                sz as usize,
                "JMPREL",
                &dynsyms_str,
                &dynsyms,
                deps_list.as_slice(),
                reloc_cache,
            )?;
        }
        // Every table processed without error: commit the recorded memo (an error path above
        // returns early and drops the partial recording instead).
        if let Some((src, rec)) = reloc_cache.record.take() {
            if let Ok(mut memos) = self.reloc_memo.lock() {
                memos.insert(src, Arc::new(rec));
            }
        }
        reloc_cache.memo = None;

        // Three-way split. `relr` and `apply` are pure memory writes into the data object (and the
        // COW faults they trigger); `resolve` is symbol lookup and is the only part further
        // lookup optimization can shrink.
        let _rels_total = _rels_start.elapsed();
        let _resolve = reloc_cache.resolve_time - _resolve_0;
        let _apply = _rels_total.saturating_sub(_resolve);
        tracing::debug!(
            "reloc {}: prep {}us, relr {}us, resolve {}us, apply {}us, {} lookups, {} cached",
            lib.name,
            (_start_2 - _start_1).as_micros(),
            _relr_time.as_micros(),
            _resolve.as_micros(),
            _apply.as_micros(),
            reloc_cache.misses - _misses_0,
            reloc_cache.hits - _hits_0,
        );

        Ok(())
    }

    fn relocate_recursive(
        &self,
        root_id: LibraryId,
        reloc_cache: &mut RelocCache<'_>,
    ) -> Result<(), DynlinkError> {
        let lib = self.get_library(root_id)?;
        let libname = lib.name.to_string();
        match lib.reloc_state.get() {
            crate::library::RelocState::Unrelocated => {}
            crate::library::RelocState::PartialRelocation => {
                error!("{}: tried to relocate a failed library", lib);
                return Err(DynlinkErrorKind::RelocationFail {
                    library: lib.name.as_str().into(),
                }
                .into());
            }
            crate::library::RelocState::Relocated => {
                trace!("{}: already relocated", lib);
                return Ok(());
            }
        }

        // We do this recursively instead of using a traversal, since we want to be able to prune
        // nodes that we know we no longer need to relocate. But since the reloc state gets
        // set at the end (so we can do this pruning), we'll need to track the visit states.
        // In the end, this is depth-first postorder.
        let deps = self
            .library_deps
            .neighbors_directed(root_id.0, petgraph::Direction::Outgoing)
            .collect::<Vec<_, SMALL_VEC_SIZE>>();

        let mut visit_state = HashSet::new();
        visit_state.insert(root_id.0);
        let rets = deps.into_iter().map(|dep_id| {
            if !visit_state.contains(&dep_id) {
                visit_state.insert(dep_id);
                self.relocate_recursive(LibraryId(dep_id), reloc_cache)
            } else {
                Ok(())
            }
        });

        DynlinkError::collect(
            DynlinkErrorKind::DepsRelocFail {
                library: libname.into(),
            },
            rets,
        )?;

        // Okay, deps are ready, let's reloc the root.
        let lib = self.get_library(root_id)?;
        lib.reloc_state.set(RelocState::PartialRelocation);

        let res = self.relocate_single(root_id, reloc_cache);

        let lib = self.get_library(root_id)?;
        if res.is_ok() {
            lib.reloc_state.set(RelocState::Relocated);
        } else {
            lib.reloc_state.set(RelocState::PartialRelocation);
        }
        res
    }

    /// Iterate through all libraries and process relocations for any libraries that haven't yet
    /// been relocated.
    pub fn relocate_all(&self, root_id: LibraryId) -> Result<(), DynlinkError> {
        let name: SmallString<[u8; SMALL_STRING_SIZE]> =
            self.get_library(root_id)?.name.as_str().into();
        let rootname = self.get_library(root_id)?.name.clone();
        // See `Context::reloc_lock`: relocations serialize against each other, but not against
        // readers of the context.
        let _reloc_guard = self.reloc_lock.lock().map_err(|_| {
            DynlinkError::new(DynlinkErrorKind::RelocationFail {
                library: name.clone(),
            })
        })?;
        let _start = Instant::now();
        let mut reloc_cache = RelocCache::default();
        let res = self
            .relocate_recursive(root_id, &mut reloc_cache)
            .map_err(|e| {
                DynlinkError::new_collect(
                    DynlinkErrorKind::RelocationFail { library: name },
                    vec![e],
                )
            });
        tracing::debug!(
            "reloc_all {}: {}us total, {}us resolve, {} lookups, {} cached, {} memo, {} memo_bad",
            rootname,
            _start.elapsed().as_micros(),
            reloc_cache.resolve_time.as_micros(),
            reloc_cache.misses,
            reloc_cache.hits,
            reloc_cache.memo_hits,
            reloc_cache.memo_bad,
        );
        // Engagement + soundness evidence for verify runs (`RELOCMEM`): replayed / disagreed /
        // live misses / cache hits. The debug line above is invisible in a release boot, and a
        // green sweep with zero replays would otherwise read as "memo verified" when the truth is
        // "memo never ran". Anon variant: dynlink links into bootstrap (see `record_on_anon`).
        secgate::statlog::record_on_anon(
            RELOC_MEMO_VERIFY,
            "RELOCMEM",
            reloc_cache.memo_hits as u64,
            &[
                reloc_cache.memo_bad as u64,
                reloc_cache.misses as u64,
                reloc_cache.hits as u64,
            ],
        );
        // A verify-mode disagreement is a memoization soundness bug: never silent, whatever the
        // logging level.
        if RELOC_MEMO_VERIFY && reloc_cache.memo_bad > 0 {
            tracing::error!(
                "reloc_all {}: RELOC MEMO VERIFY FAILED: {} replayed resolutions disagreed with \
                 live lookup",
                rootname,
                reloc_cache.memo_bad,
            );
        }
        res
    }
}
