//! This crate provides basic interfaces for working with the compositor and display manager.
//!
//! Currently, you can create and manage windows, and fill their framebuffers, and flip that
//! to the compositor. This interface is unstable, and will be changing.

pub use display_core::*;
use secgate::util::Handle;
use twizzler::object::{MapFlags, Object};
use twizzler_rt_abi::error::TwzError;

#[repr(C)]
/// A window handle for the compositor. Provides a compositing buffer.
pub struct WindowHandle {
    key: u32,
    pub window_buffer: BufferObject,
}

impl WindowHandle {
    /// Change window parameters, such as position, size, or z-sorting.
    pub fn reconfigure(&self, wconfig: WindowConfig) -> Result<(), TwzError> {
        display_srv::reconfigure_window(self.key, wconfig)
    }

    /// Get the current window configuration.
    pub fn get_config(&self) -> Result<WindowConfig, TwzError> {
        display_srv::get_window_config(self.key)
    }
}

impl Drop for WindowHandle {
    fn drop(&mut self) {
        self.release();
    }
}

impl Handle for WindowHandle {
    type OpenError = TwzError;

    type OpenInfo = WindowConfig;

    fn open(info: Self::OpenInfo) -> Result<Self, Self::OpenError>
    where
        Self: Sized,
    {
        let (id, key) = display_srv::create_window(info)?;
        Ok(WindowHandle {
            key,
            window_buffer: BufferObject::from(unsafe {
                Object::map_unchecked(id, MapFlags::READ | MapFlags::WRITE)
            }?),
        })
    }

    fn release(&mut self) {
        let _ = display_srv::drop_window(self.key);
    }
}

/// Get the current display information. Current resolution is returned in
/// the width and height fields.
pub fn get_display_info() -> Result<WindowConfig, TwzError> {
    display_srv::get_display_info()
}
