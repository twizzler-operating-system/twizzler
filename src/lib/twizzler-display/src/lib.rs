pub use display_core::*;
use secgate::util::Handle;
use twizzler::object::{MapFlags, Object};
use twizzler_rt_abi::error::TwzError;

#[repr(C)]
pub struct WindowHandle {
    key: u32,
    pub window_buffer: BufferObject,
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
