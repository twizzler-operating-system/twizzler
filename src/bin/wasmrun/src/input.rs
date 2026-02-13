//! Input device integration for wasmrun.
//!
//! Optionally initializes the virtio-input driver and provides event polling.
//! If no input device is found (e.g., QEMU started without virtio-keyboard-pci),
//! operations gracefully return empty results.

use std::sync::{Mutex, OnceLock};

use virtio_input::{InputDevice, InputTransport, VirtioInputEvent};

struct InputState {
    device: InputDevice<InputTransport>,
}

static INPUT: OnceLock<Mutex<InputState>> = OnceLock::new();

/// Attempt to initialize the input device. Returns true if successful.
/// Safe to call multiple times — only the first call does anything.
pub fn init() -> bool {
    if INPUT.get().is_some() {
        return true;
    }

    let (send, _recv) = std::sync::mpsc::channel();
    match virtio_input::get_device(send) {
        Ok(device) => {
            eprintln!("[input] virtio-input device initialized");
            let _ = INPUT.set(Mutex::new(InputState { device }));
            true
        }
        Err(e) => {
            eprintln!("[input] no input device found: {e:?}");
            false
        }
    }
}

/// Poll for all pending input events. Returns an empty vec if no device
/// is available or no events are pending.
pub fn poll_events() -> Vec<VirtioInputEvent> {
    let Some(state) = INPUT.get() else {
        return Vec::new();
    };
    let state = state.lock().unwrap();
    let mut events = Vec::new();
    while let Some(event) = state.device.pop_event() {
        events.push(event);
    }
    events
}
