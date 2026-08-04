//! `Effects` backed by real Twizzler objects.
//!
//! One name maps to one object holding raw bytes. Objects are created through
//! `ObjectBuilder`, registered with the naming service so they can be found
//! again, and re-opened by ID on subsequent runs.
//!
//! This is the only file in the crate that knows Twizzler exists.

use anyhow::{anyhow, bail, Result};
use naming::{dynamic_naming_factory, DynamicNamingHandle, GetFlags};
use twizzler::{
    collections::vec::{Vec as TwzVec, VecObject, VecObjectAlloc},
    object::{Object, ObjectBuilder},
};
use twizzler_rt_abi::{
    error::TwzError,
    object::{MapFlags, ObjID},
};

use crate::effects::{Effects, Handle};

/// An object holding a byte blob.
type Blob = VecObject<u8, VecObjectAlloc>;
type BlobBase = TwzVec<u8, VecObjectAlloc>;

/// Objects are volatile: this milestone needs them to outlive the loop, not the
/// boot, and volatile keeps the pager out of the picture. Add `PERSIST` here and
/// `.persist(true)` on the builder together if that changes.
const MAP: MapFlags = MapFlags::READ.union(MapFlags::WRITE);

/// Twizzler errors don't implement `std::error::Error`, so carry them across as
/// text rather than `?`-ing them into `anyhow`.
fn twz<T, E: core::fmt::Debug>(what: &str, r: core::result::Result<T, E>) -> Result<T> {
    r.map_err(|e| anyhow!("{what}: {e:?}"))
}

/// Prefix for every object this harness owns.
///
/// Flat, and in the root namespace: naming-srv only creates `/initrd` at boot,
/// so anything of the form `/foo/bar` would need its namespace created first.
pub const DEFAULT_PREFIX: &str = "llm-harness-";

pub struct TwizzlerEffects {
    namer: DynamicNamingHandle,
    prefix: String,
}

impl TwizzlerEffects {
    pub fn new(prefix: impl Into<String>) -> Result<Self> {
        let namer =
            dynamic_naming_factory().ok_or_else(|| anyhow!("naming service unavailable"))?;
        Ok(Self {
            namer,
            prefix: prefix.into(),
        })
    }

    /// Flatten a logical path onto the prefix, in the root namespace (see the
    /// module doc). `src/greet.rs` becomes `llm-harness-src%2Fgreet.rs`.
    ///
    /// `/` is percent-encoded rather than replaced with a filler character so
    /// the mapping stays injective — `src/greet.rs` and `src_greet.rs` must
    /// not collide on one object. `.` and `..` path components are rejected
    /// outright: they are meaningless once flattened, and silently accepting
    /// them here would be a foot-gun for any future backend that maps names
    /// onto a real hierarchical filesystem instead.
    fn path(&self, name: &str) -> Result<String> {
        let stripped = name.strip_prefix('/').unwrap_or(name);
        if stripped.is_empty() {
            bail!("empty object name");
        }
        for part in stripped.split('/') {
            if part.is_empty() || part == "." || part == ".." {
                bail!("invalid object name {name:?}: empty, '.', or '..' path component");
            }
        }
        let escaped = stripped.replace('%', "%25").replace('/', "%2F");
        Ok(format!("{}{}", self.prefix, escaped))
    }

    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    fn map(&self, id: ObjID) -> Result<Blob> {
        let obj = twz("map object", Object::<BlobBase>::map(id, MAP))?;
        Ok(VecObject::from(obj))
    }
}

impl Effects for TwizzlerEffects {
    /// Resolve an existing object by name. Errs if it does not exist — a
    /// naming-service hiccup, permission error, or malformed path is
    /// propagated rather than silently treated as "not found and safe to
    /// recreate," which would orphan whatever the name used to point at.
    fn open_read(&mut self, name: &str) -> Result<Handle> {
        let path = self.path(name)?;
        match self.namer.get(&path, GetFlags::empty()) {
            Ok(node) => Ok(Handle::from_raw(node.id.raw())),
            Err(e) if e == TwzError::NOT_FOUND => Err(anyhow!("no such object: {name}")),
            Err(e) => Err(anyhow!("resolve {path}: {e:?}")),
        }
    }

    /// Resolve a name to its object, creating an empty one only if the
    /// lookup specifically reports "not found." Any other error (naming
    /// hiccup, permission issue, malformed path) is propagated instead of
    /// falling through to create-and-rebind, which would orphan the
    /// previous object and hand the caller a silent empty file.
    fn open_write(&mut self, name: &str) -> Result<Handle> {
        let path = self.path(name)?;

        match self.namer.get(&path, GetFlags::empty()) {
            Ok(node) => return Ok(Handle::from_raw(node.id.raw())),
            Err(e) if e == TwzError::NOT_FOUND => {}
            Err(e) => return Err(anyhow!("resolve {path}: {e:?}")),
        }

        let blob: Blob = twz("create object", VecObject::new(ObjectBuilder::default()))?;
        let id = blob.object().id();
        twz("name object", self.namer.put(&path, id))?;
        Ok(Handle::from_raw(id.raw()))
    }

    fn read(&mut self, h: Handle) -> Result<Vec<u8>> {
        let blob = self.map(ObjID::new(h.raw()))?;
        let slice = blob.as_slice();
        Ok(slice.as_slice().to_vec())
    }

    fn write(&mut self, h: Handle, b: &[u8]) -> Result<()> {
        let mut blob = self.map(ObjID::new(h.raw()))?;
        twz("clear object", blob.clear())?;
        twz("append to object", blob.append(b.iter().copied()))?;
        Ok(())
    }
}
