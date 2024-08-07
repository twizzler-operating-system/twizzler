use monitor_api::SharedCompConfig;
use talc::Span;
use twizzler_abi::object::{MAX_SIZE, NULLPAGE_SIZE};

use crate::mon::space::MapHandle;

/// Manages a comp config object.
pub struct CompConfigObject {
    handle: MapHandle,
}

impl CompConfigObject {
    /// Create a new CompConfigObject from a handle. Initializes the comp config.
    pub fn new(handle: MapHandle, init_val: SharedCompConfig) -> Self {
        let mut this = Self { handle };
        this.write_config(init_val);
        this
    }

    /// Write a comp config to this object.
    pub fn write_config(&mut self, val: SharedCompConfig) {
        // Safety: only the monitor can write to a comp config object, and we have a mutable
        // reference to it.
        unsafe {
            let base = self.handle.monitor_data_base();
            (base as *mut SharedCompConfig).write(val);
        }
    }

    /// Read the comp config data.
    pub(crate) fn read_comp_config(&self) -> SharedCompConfig {
        // Safety: no other compartment can write this.
        unsafe { self.get_comp_config().read() }
    }

    /// Get a pointer to this comp config data.
    pub fn get_comp_config(&self) -> *const SharedCompConfig {
        self.handle.monitor_data_base() as *const SharedCompConfig
    }

    /// Return a span that can be used for allocations in the comp config object.
    pub fn alloc_span(&self) -> Span {
        let offset_from_base =
            core::mem::size_of::<SharedCompConfig>().next_multiple_of(NULLPAGE_SIZE);
        assert!(offset_from_base < MAX_SIZE / 2);
        // Safety: the pointers stay in-bounds (in an object).
        unsafe {
            Span::new(
                self.handle.monitor_data_base().add(offset_from_base),
                self.handle.monitor_data_null().add(MAX_SIZE / 2),
            )
        }
    }
}
