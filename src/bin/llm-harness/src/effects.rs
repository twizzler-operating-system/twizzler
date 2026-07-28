//! The only way the agent loop touches the outside world.
//!
//! Every backend — the in-memory stub here, `TwizzlerEffects` later — sits
//! behind this trait. The loop must never reach past it.

use std::collections::HashMap;

use anyhow::{anyhow, Result};

/// An opaque reference to something openable.
///
/// The inner value is backend-defined: an index for the stub, an object ID for
/// Twizzler. Callers only pass it back to the `Effects` that produced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Handle(u128);

impl Handle {
    pub(crate) fn from_raw(raw: u128) -> Self {
        Self(raw)
    }

    pub(crate) fn raw(self) -> u128 {
        self.0
    }
}

pub trait Effects {
    fn read(&mut self, h: Handle) -> Result<Vec<u8>>;
    fn write(&mut self, h: Handle, b: &[u8]) -> Result<()>;
    fn open(&mut self, name: &str) -> Result<Handle>;
}

/// In-memory stub. Host-side tests only.
#[derive(Debug, Default)]
pub struct MemEffects {
    names: HashMap<String, Handle>,
    blobs: HashMap<Handle, Vec<u8>>,
    next: u128,
}

impl MemEffects {
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed an object so a test can `Read` it.
    pub fn preload(&mut self, name: &str, body: impl Into<Vec<u8>>) {
        let h = self.alloc(name);
        self.blobs.insert(h, body.into());
    }

    /// Contents by name, for assertions.
    pub fn get(&self, name: &str) -> Option<&[u8]> {
        let h = self.names.get(name)?;
        self.blobs.get(h).map(|v| v.as_slice())
    }

    /// Every name that exists, sorted.
    pub fn names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.names.keys().map(|s| s.as_str()).collect();
        names.sort_unstable();
        names
    }

    fn alloc(&mut self, name: &str) -> Handle {
        if let Some(h) = self.names.get(name) {
            return *h;
        }
        self.next += 1;
        let h = Handle::from_raw(self.next);
        self.names.insert(name.to_string(), h);
        self.blobs.insert(h, Vec::new());
        h
    }
}

impl Effects for MemEffects {
    fn read(&mut self, h: Handle) -> Result<Vec<u8>> {
        self.blobs
            .get(&h)
            .cloned()
            .ok_or_else(|| anyhow!("no object for handle {}", h.raw()))
    }

    fn write(&mut self, h: Handle, b: &[u8]) -> Result<()> {
        let slot = self
            .blobs
            .get_mut(&h)
            .ok_or_else(|| anyhow!("no object for handle {}", h.raw()))?;
        slot.clear();
        slot.extend_from_slice(b);
        Ok(())
    }

    /// Opens by name, creating an empty object if it does not exist.
    fn open(&mut self, name: &str) -> Result<Handle> {
        Ok(self.alloc(name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_write_read_round_trip() {
        let mut e = MemEffects::new();
        let h = e.open("a.rs").unwrap();
        e.write(h, b"hello").unwrap();
        assert_eq!(e.read(h).unwrap(), b"hello");
        assert_eq!(e.get("a.rs").unwrap(), b"hello");
    }

    #[test]
    fn open_is_stable_for_a_name() {
        let mut e = MemEffects::new();
        assert_eq!(e.open("x").unwrap(), e.open("x").unwrap());
        assert_ne!(e.open("x").unwrap(), e.open("y").unwrap());
    }

    #[test]
    fn write_replaces_rather_than_appends() {
        let mut e = MemEffects::new();
        let h = e.open("x").unwrap();
        e.write(h, b"long content").unwrap();
        e.write(h, b"short").unwrap();
        assert_eq!(e.read(h).unwrap(), b"short");
    }

    #[test]
    fn preload_is_readable() {
        let mut e = MemEffects::new();
        e.preload("task.md", "do the thing");
        let h = e.open("task.md").unwrap();
        assert_eq!(e.read(h).unwrap(), b"do the thing");
    }

    #[test]
    fn unknown_handle_errors() {
        let mut e = MemEffects::new();
        assert!(e.read(Handle::from_raw(999)).is_err());
        assert!(e.write(Handle::from_raw(999), b"x").is_err());
    }
}
