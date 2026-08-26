//! Management of global context.

use std::{
    collections::HashMap,
    fmt::Display,
    sync::{Arc, Mutex},
};

use petgraph::stable_graph::{NodeIndex, StableDiGraph};
use stable_vec::StableVec;
use twizzler_abi::object::ObjID;

use crate::{
    compartment::{Compartment, CompartmentId},
    engines::ContextEngine,
    library::{Library, LibraryId, UnloadedLibrary},
    DynlinkError, DynlinkErrorKind,
};

mod deps;
mod load;
pub(crate) mod relocate;
pub mod runtime;
mod syms;

pub use load::LoadIds;

/// Switch for the symbol-indexing counter (`SYMINDEX`): microseconds and symbol count per library.
///
/// Measured: was 67% of all library-load time at 871 ns/symbol, when the map owned its keys. With
/// the hash key it is 40% at 556 ns/symbol. See `COMPNEW.md`.
///
/// A companion `SYMFALL` counter established that the global fallback fires ~7416 times a run
/// against ~309 library loads -- 24 to 1 -- which is why this index is built eagerly rather than
/// lazily. It was removed after answering that, being loud enough to move the timings it shared a
/// run with.
const SYM_INDEX_STATS: bool = false;

/// Switch for the secgate-name-set counter (`SGNAMES`): microseconds and gate count per *build*.
///
/// Measured and closed. `Library::secgate_names` is derived per instance from what is really a
/// property of the source object -- the same shape as the symbol index, which was 88% repeat work
/// -- so sharing it per object looked like the same win. It is not: the set is built lazily and
/// only for libraries that actually export gates, so it is built ~8 times a run costing 0.09 ms
/// total. Sharing it per source object changed the build count not at all (25 vs 27 over three
/// runs). The shape matched; the volume did not, because `index_library_symbols` ran eagerly for
/// every library and this does not.
pub(crate) const SGNAME_STATS: bool = false;

/// FNV-1a over a symbol name: the key for [Context::sym_index].
///
/// Must stay identical between insert and lookup; nothing else depends on the value.
pub(crate) fn sym_hash(name: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in name.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

#[repr(C)]
/// A dynamic linker context, the main state struct for this crate.
pub struct Context {
    // Implementation callbacks.
    pub engine: Box<dyn ContextEngine + Send>,
    // Track all the compartment names.
    compartment_names: HashMap<String, usize>,
    // Compartments get stable IDs from StableVec.
    compartments: StableVec<Compartment>,

    // This is the primary list of libraries, all libraries have an entry here, and they are
    // placed here independent of compartment. Edges denote dependency relationships, and may also
    // cross compartments.
    pub(crate) library_deps: StableDiGraph<LoadedOrUnloaded, ()>,

    // One approximate membership filter per *source ELF object*, telling the global fallback
    // which libraries could define a name. Replaces a name -> instances index that was rebuilt for
    // every compartment: loading a repeat instance now costs one `Arc` clone instead of an insert
    // per symbol, and nothing here grows with the number of live compartments.
    //
    // TODO: an ObjID can be reused, so this should be keyed by a fingerprint of the object's
    // contents rather than its ID alone. Safe for read-only `.so` objects within one boot, which
    // is what exists today.
    sym_blooms: HashMap<ObjID, Arc<SymBloom>>,

    // Memoized symbol resolutions per library source object (see `relocate::RELOC_MEMO`). Only
    // mutated under `reloc_lock` (relocations are serialized), so this Mutex is uncontended; it
    // exists because relocation runs under a shared `&Context`. Same ObjID-reuse caveat as
    // `sym_blooms`.
    pub(crate) reloc_memo: Mutex<HashMap<ObjID, Arc<relocate::LibReplayMemo>>>,

    /// Times `do_lookup_symbol` fell through to the global search (a walk over every library
    /// node). Relaxed; read as per-`relocate_all` deltas by the `RELOCMEM` sizing record. The
    /// removed `SYMFALL` counter measured this at ~24 per library load -- the prior this exists
    /// to re-check on the current tree.
    pub(crate) global_fallbacks: std::sync::atomic::AtomicU64,

    // Relocation runs under a shared reference, so it is no longer serialized by the caller's
    // write lock. Two concurrent relocations of a shared dependency would race on its
    // `reloc_state`, and the loser would observe `PartialRelocation` and report the library as
    // failed. This restores exactly the mutual exclusion the write lock used to provide, and
    // nothing else takes it, so readers still proceed during relocation.
    pub(crate) reloc_lock: Mutex<()>,
}

/// Approximate set of the symbol names one ELF object defines.
///
/// Built once per object and shared by every compartment that loads it. A false positive costs one
/// rejected `Library::lookup_symbol`, which the fallback already does per candidate; false
/// negatives cannot occur, which is the only direction that would be wrong.
pub(crate) struct SymBloom {
    bits: std::vec::Vec<u64>,
    /// Bit-index mask; `bits.len() * 64` is a power of two.
    mask: u64,
}

impl SymBloom {
    const K: usize = 3;
    /// ~16 bits per symbol at k=3 puts the false-positive rate near 0.3%.
    const BITS_PER_SYM: usize = 16;

    fn new(nsyms: usize) -> Self {
        let nbits = (nsyms.max(1) * Self::BITS_PER_SYM)
            .next_power_of_two()
            .max(64);
        Self {
            bits: std::vec![0; nbits / 64],
            mask: nbits as u64 - 1,
        }
    }

    /// Three bit positions from one hash, by double hashing.
    fn positions(&self, hash: u64) -> [u64; Self::K] {
        let mut h = hash;
        let mut out = [0u64; Self::K];
        for slot in out.iter_mut() {
            *slot = h & self.mask;
            h = (h ^ (h >> 33)).wrapping_mul(0xff51afd7ed558ccd);
        }
        out
    }

    fn insert(&mut self, hash: u64) {
        for pos in self.positions(hash) {
            self.bits[(pos / 64) as usize] |= 1 << (pos % 64);
        }
    }

    pub(crate) fn maybe_contains(&self, hash: u64) -> bool {
        self.positions(hash)
            .iter()
            .all(|pos| self.bits[(pos / 64) as usize] & (1 << (pos % 64)) != 0)
    }
}

// Libraries in the dependency graph are placed there before loading, so that they can participate
// in dependency search. So we need to track both kinds of libraries that may be at a given index in
// the graph.
pub enum LoadedOrUnloaded {
    Unloaded(UnloadedLibrary),
    Loaded(Library),
}

impl Display for LoadedOrUnloaded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadedOrUnloaded::Unloaded(unlib) => write!(f, "(unloaded){}", unlib),
            LoadedOrUnloaded::Loaded(lib) => write!(f, "(loaded){}", lib),
        }
    }
}

impl LoadedOrUnloaded {
    /// Get the name of this library, loaded or unloaded.
    pub fn name(&self) -> &str {
        match self {
            LoadedOrUnloaded::Unloaded(unlib) => &unlib.name,
            LoadedOrUnloaded::Loaded(lib) => &lib.name,
        }
    }

    /// Get back a reference to the underlying loaded library, if loaded.
    pub fn loaded(&self) -> Option<&Library> {
        match self {
            LoadedOrUnloaded::Unloaded(_) => None,
            LoadedOrUnloaded::Loaded(lib) => Some(lib),
        }
    }

    /// Get back a mutable reference to the underlying loaded library, if loaded.
    pub fn loaded_mut(&mut self) -> Option<&mut Library> {
        match self {
            LoadedOrUnloaded::Unloaded(_) => None,
            LoadedOrUnloaded::Loaded(lib) => Some(lib),
        }
    }
}

impl Context {
    /// Construct a new dynamic linker context.
    pub fn new(engine: Box<dyn ContextEngine + Send>) -> Self {
        Self {
            engine,
            compartment_names: HashMap::new(),
            library_deps: StableDiGraph::new(),
            compartments: StableVec::new(),
            sym_blooms: HashMap::new(),
            reloc_memo: Mutex::new(HashMap::new()),
            global_fallbacks: std::sync::atomic::AtomicU64::new(0),
            reloc_lock: Mutex::new(()),
        }
    }

    /// Give a freshly-loaded library the membership filter for its ELF object, building it the
    /// first time that object is seen.
    pub(crate) fn index_library_symbols(&mut self, idx: NodeIndex) {
        let _start = std::time::Instant::now();
        let Some(src_id) = self.library_deps[idx].loaded().map(|lib| lib.full_obj.id()) else {
            return;
        };
        // Repeat instances take this branch: one `Arc` clone, no per-symbol work at all.
        let (bloom, _nsyms) = match self.sym_blooms.get(&src_id) {
            Some(bloom) => (bloom.clone(), 0),
            None => {
                let Some((bloom, nsyms)) = self.build_sym_bloom(idx) else {
                    return;
                };
                self.sym_blooms.insert(src_id, bloom.clone());
                (bloom, nsyms)
            }
        };
        if let Some(lib) = self.library_deps[idx].loaded_mut() {
            lib.sym_bloom = Some(bloom);
        }
        secgate::statlog::record_on_anon(
            SYM_INDEX_STATS,
            "SYMINDEX",
            _start.elapsed().as_nanos() as u64 / 1000,
            &[_nsyms as u64],
        );
    }

    /// Walk an ELF object's dynamic symbols once, filling a filter with every name it defines.
    fn build_sym_bloom(&self, idx: NodeIndex) -> Option<(Arc<SymBloom>, usize)> {
        let lib = self.library_deps[idx].loaded()?;
        let common = lib.get_elf_common().ok()?;
        let (syms, strs) = (common.dynsyms.as_ref()?, common.dynsyms_strs.as_ref()?);
        let defined = syms.iter().filter(|sym| !sym.is_undefined()).count();
        let mut bloom = SymBloom::new(defined * 2);
        let mut nsyms = 0;
        for sym in syms.iter().filter(|sym| !sym.is_undefined()) {
            let Ok(name) = strs.get(sym.st_name as usize) else {
                continue;
            };
            nsyms += 1;
            // Both spellings, mirroring the prefixed retry in Library::lookup_symbol.
            if let Some(bare) = name.strip_prefix("__TWIZZLER_SECURE_GATE_") {
                bloom.insert(sym_hash(bare));
            }
            bloom.insert(sym_hash(name));
        }
        Some((Arc::new(bloom), nsyms))
    }

    /// Replace the callback engine for this context.
    pub fn replace_engine(&mut self, engine: Box<dyn ContextEngine + Send>) {
        self.engine = engine;
    }

    /// Lookup a compartment by name.
    pub fn lookup_compartment(&self, name: &str) -> Option<CompartmentId> {
        Some(CompartmentId(*self.compartment_names.get(name)?))
    }

    /// Get a reference to a compartment back by ID.
    pub fn get_compartment(&self, id: CompartmentId) -> Result<&Compartment, DynlinkError> {
        if !self.compartments.has_element_at(id.0) {
            return Err(DynlinkErrorKind::InvalidCompartmentId { id }.into());
        }
        Ok(&self.compartments[id.0])
    }

    /// Get a mut reference to a compartment back by ID.
    pub fn get_compartment_mut(
        &mut self,
        id: CompartmentId,
    ) -> Result<&mut Compartment, DynlinkError> {
        if !self.compartments.has_element_at(id.0) {
            return Err(DynlinkErrorKind::InvalidCompartmentId { id }.into());
        }
        Ok(&mut self.compartments[id.0])
    }

    /// Lookup a library by name
    pub fn lookup_library(&self, comp: CompartmentId, name: &str) -> Option<LibraryId> {
        let comp = self.get_compartment(comp).ok()?;
        Some(LibraryId(*comp.library_names.get(name)?))
    }

    /// Get a reference to a library back by ID.
    pub fn get_library(&self, id: LibraryId) -> Result<&Library, DynlinkError> {
        if !self.library_deps.contains_node(id.0) {
            return Err(DynlinkErrorKind::InvalidLibraryId { id }.into());
        }
        match &self.library_deps[id.0] {
            LoadedOrUnloaded::Unloaded(unlib) => Err(DynlinkErrorKind::UnloadedLibrary {
                library: unlib.name.as_str().into(),
            }
            .into()),
            LoadedOrUnloaded::Loaded(lib) => Ok(lib),
        }
    }

    /// Get a mut reference to a library back by ID.
    pub fn get_library_mut(&mut self, id: LibraryId) -> Result<&mut Library, DynlinkError> {
        if !self.library_deps.contains_node(id.0) {
            return Err(DynlinkErrorKind::InvalidLibraryId { id }.into());
        }
        match &mut self.library_deps[id.0] {
            LoadedOrUnloaded::Unloaded(unlib) => Err(DynlinkErrorKind::UnloadedLibrary {
                library: unlib.name.as_str().into(),
            }
            .into()),
            LoadedOrUnloaded::Loaded(lib) => Ok(lib),
        }
    }

    /// Traverse the library graph with DFS postorder, calling the callback for each library.
    pub fn with_dfs_postorder<R>(
        &self,
        root_id: LibraryId,
        mut f: impl FnMut(&LoadedOrUnloaded) -> R,
    ) -> Vec<R> {
        let mut rets = vec![];
        let mut visit = petgraph::visit::DfsPostOrder::new(&self.library_deps, root_id.0);
        while let Some(node) = visit.next(&self.library_deps) {
            let dep = &self.library_deps[node];
            rets.push(f(dep))
        }
        rets
    }

    /// Traverse the library graph with DFS postorder, calling the callback for each library
    /// (mutable ref).
    pub fn with_dfs_postorder_mut<R>(
        &mut self,
        root_id: LibraryId,
        mut f: impl FnMut(&mut LoadedOrUnloaded) -> R,
    ) -> Vec<R> {
        let mut rets = vec![];
        let mut visit = petgraph::visit::DfsPostOrder::new(&self.library_deps, root_id.0);
        while let Some(node) = visit.next(&self.library_deps) {
            let dep = &mut self.library_deps[node];
            rets.push(f(dep))
        }
        rets
    }

    /// Traverse the library graph with BFS, calling the callback for each library.
    pub fn with_bfs(&self, root_id: LibraryId, mut f: impl FnMut(&LoadedOrUnloaded) -> bool) {
        let mut visit = petgraph::visit::Bfs::new(&self.library_deps, root_id.0);
        while let Some(node) = visit.next(&self.library_deps) {
            let dep = &self.library_deps[node];
            if !f(dep) {
                return;
            }
        }
    }

    pub fn libraries(&self) -> LibraryIter<'_> {
        LibraryIter { ctx: self, next: 0 }
    }

    pub(crate) fn add_library(&mut self, lib: UnloadedLibrary) -> NodeIndex {
        self.library_deps.add_node(LoadedOrUnloaded::Unloaded(lib))
    }

    pub(crate) fn add_dep(&mut self, parent: NodeIndex, dep: NodeIndex) {
        self.library_deps.add_edge(parent, dep, ());
    }

    pub fn add_manual_dependency(&mut self, parent: LibraryId, dependee: LibraryId) {
        self.add_dep(parent.0, dependee.0);
    }

    pub fn unload_compartment(
        &mut self,
        comp_id: CompartmentId,
    ) -> (Option<Compartment>, Vec<LoadedOrUnloaded>) {
        let Ok(comp) = self.get_compartment(comp_id) else {
            return (None, vec![]);
        };
        let name = comp.name.clone();
        let ids = comp.library_ids();
        let nodes = ids
            .collect::<Vec<_>>()
            .iter()
            // No index to unwind: a library's filter is an `Arc` it owns, so removing the node
            // drops it. Previously this had to scan the whole name -> instances map per library.
            .filter_map(|id| self.library_deps.remove_node(id.0))
            .collect();
        self.compartment_names.remove(&name);
        (self.compartments.remove(comp_id.0), nodes)
    }

    /// Create a new compartment with a given name.
    pub fn add_compartment(
        &mut self,
        name: impl ToString,
        new_comp_flags: NewCompartmentFlags,
    ) -> Result<CompartmentId, DynlinkError> {
        let name = name.to_string();
        let idx = self.compartments.next_push_index();
        let comp = Compartment::new(name.clone(), CompartmentId(idx), new_comp_flags);
        self.compartments.push(comp);
        tracing::debug!("added compartment {} with ID {}", name, idx);
        self.compartment_names.insert(name, idx);
        Ok(CompartmentId(idx))
    }

    /// Get a list of external compartments that the given compartment depends on.
    pub fn compartment_dependencies(
        &self,
        id: CompartmentId,
    ) -> Result<Vec<CompartmentId>, DynlinkError> {
        let comp = self.get_compartment(id)?;
        let mut deps = vec![];
        for lib in comp.library_ids() {
            for n in self.library_deps.neighbors(lib.0) {
                let neigh = self.library_deps[n].loaded().unwrap();
                deps.push(neigh.comp_id);
            }
        }
        deps.sort_unstable();
        deps.dedup();
        if let Some(dep) = deps.iter().position(|dep| *dep == id) {
            deps.remove(dep);
        }
        Ok(deps)
    }
}

pub struct LibraryIter<'a> {
    ctx: &'a Context,
    next: usize,
}

impl<'a> Iterator for LibraryIter<'a> {
    type Item = &'a Library;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let idx = self.ctx.library_deps.node_indices().nth(self.next)?;
            self.next += 1;
            let node = &self.ctx.library_deps[idx];
            match node {
                LoadedOrUnloaded::Unloaded(_) => {}
                LoadedOrUnloaded::Loaded(lib) => return Some(lib),
            }
        }
    }
}

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug)]
    pub struct NewCompartmentFlags : u32 {
        const EXPORT_GATES = 0x1;
        const DEBUG = 0x2;
    }
}
