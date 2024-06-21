#![feature(fn_traits)]
#![feature(unboxed_closures)]
#![feature(tuple_trait)]
#![feature(naked_functions)]
#![feature(auto_traits)]
#![feature(negative_impls)]
#![feature(linkage)]
#![feature(core_intrinsics)]

use core::ffi::CStr;
use std::{cell::UnsafeCell, marker::Tuple, mem::MaybeUninit};

pub use secgate_macros::*;
use twizzler_abi::object::ObjID;

/// Enum of possible return codes, similar to [Result], but with specific
/// variants of possible failures of initializing or invoking the secure gate call.
#[derive(Debug, Copy, Clone, Eq, PartialEq, PartialOrd, Ord, Hash)]
#[repr(C, u32)]
pub enum SecGateReturn<T> {
    /// Call succeeded, and returned T.
    Success(T),
    /// Permission was denied for this call.
    PermissionDenied,
    /// The callee panic'd inside the other compartment.
    CalleePanic,
    /// The call went through, but no return value was given.
    NoReturnValue,
}

impl<T> SecGateReturn<T> {
    #[track_caller]
    pub fn unwrap(self) -> T {
        match self {
            SecGateReturn::Success(data) => data,
            _ => panic!("failed to unwrap non-successful secure gate return"),
        }
    }
}

/// A struct of information about a secure gate. These are auto-generated by the
/// [crate::secure_gate] macro, and stored in a special ELF section (.twz_secgate_info) as an array.
/// The dynamic linker and monitor can then use this to easily enumerate gates.
#[repr(C)]
pub struct SecGateInfo<F> {
    /// A pointer to the implementation entry function. This must be a pointer, and we statically
    /// check that is has the same size as usize (sorry cheri, we'll fix this another time)
    pub imp: F,
    /// The name of this secure gate. This must be a pointer to a null-terminated C string.
    name: *const i8,
}

impl<F> core::fmt::Debug for SecGateInfo<F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SecGateInfo({:p})", self.name)
    }
}

impl<F> SecGateInfo<F> {
    pub const fn new(imp: F, name: &'static CStr) -> Self {
        Self {
            imp,
            name: name.as_ptr(),
        }
    }

    pub fn name(&self) -> &CStr {
        // Safety: we only ever construct self from a static CStr.
        unsafe { CStr::from_ptr(self.name) }
    }
}

// Safety: If F is Send, we are too because the name field points to a static C string that cannot
// be written to.
unsafe impl<F: Send> Send for SecGateInfo<F> {}
// Safety: If F is Sync, we are too because the name field points to a static C string that cannot
// be written to.
unsafe impl<F: Sync> Sync for SecGateInfo<F> {}

/// Minimum alignment of secure trampolines.
pub const SECGATE_TRAMPOLINE_ALIGN: usize = 0x10;

/// Non-generic and non-pointer-based SecGateInfo, for use during dynamic linking.
pub type RawSecGateInfo = SecGateInfo<usize>;
// Ensure that these are the same size because the dynamic linker uses the raw variant.
static_assertions::assert_eq_size!(RawSecGateInfo, SecGateInfo<&fn()>);

/// Arguments that will be passed to the secure call. Concrete versions of this are generated by the
/// macro.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct Arguments<Args: Tuple + Crossing + Copy> {
    args: Args,
}

impl<Args: Tuple + Crossing + Copy> Arguments<Args> {
    pub fn with_alloca<F, R>(args: Args, f: F) -> R
    where
        F: FnOnce(&mut Self) -> R,
    {
        alloca::alloca(|stack_space| {
            stack_space.write(Self { args });
            // Safety: we init the MaybeUninit just above.
            f(unsafe { stack_space.assume_init_mut() })
        })
    }

    pub fn into_inner(self) -> Args {
        self.args
    }
}

/// Return value to be filled by the secure call. Concrete versions of this are generated by the
/// macro.
#[derive(Copy)]
#[repr(C)]
pub struct Return<T: Crossing + Copy> {
    isset: bool,
    ret: MaybeUninit<T>,
}

impl<T: Copy + Crossing> Clone for Return<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: Crossing + Copy> Return<T> {
    pub fn with_alloca<F, R>(f: F) -> R
    where
        F: FnOnce(&mut Self) -> R,
    {
        alloca::alloca(|stack_space| {
            stack_space.write(Self {
                isset: false,
                ret: MaybeUninit::uninit(),
            });
            // Safety: we init the MaybeUninit just above.
            f(unsafe { stack_space.assume_init_mut() })
        })
    }

    /// If a previous call to set is made, or this was constructed by new(), then into_inner
    /// returns the inner value. Otherwise, returns None.
    pub fn into_inner(self) -> Option<T> {
        if self.isset {
            Some(unsafe { self.ret.assume_init() })
        } else {
            None
        }
    }

    /// Construct a new, uninitialized Self.
    pub fn new_uninit() -> Self {
        Self {
            isset: false,
            ret: MaybeUninit::uninit(),
        }
    }

    /// Set the inner value. Future call to into_inner will return Some(val).
    pub fn set(&mut self, val: T) {
        self.ret.write(val);
        self.isset = true;
    }
}

/// An auto trait that limits the types that can be send across to another compartment. These are:
/// 1. Types other than references, UnsafeCell, raw pointers, slices.
/// 2. #[repr(C)] structs and enums made from Crossing types.
///
/// # Safety
/// The type must meet the above requirements.
pub unsafe auto trait Crossing {}

impl<T> !Crossing for &T {}
impl<T> !Crossing for &mut T {}
impl<T> !Crossing for UnsafeCell<T> {}
impl<T> !Crossing for *const T {}
impl<T> !Crossing for *mut T {}
impl<T> !Crossing for &[T] {}
impl<T> !Crossing for &mut [T] {}

unsafe impl<T: Crossing + Copy> Crossing for SecGateReturn<T> {}

/// Required to put in your source if you call any secure gates.
// TODO: this isn't ideal, but it's the only solution I have at the moment. For some reason,
// the linker doesn't even bother linking the libcalloca.a library that alloca creates. This forces
// that to happen.
#[macro_export]
macro_rules! secgate_prelude {
    () => {
        #[link(name = "calloca", kind = "static")]
        extern "C" {
            pub fn c_with_alloca();
        }
    };
}

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Ord, Eq, Hash)]
#[repr(C)]
pub struct GateCallInfo {
    thread_id: ObjID,
    src_ctx: ObjID,
}

impl GateCallInfo {
    /// Allocate a new GateCallInfo on the stack for the closure.
    pub fn with_alloca<F, R>(thread_id: ObjID, src_ctx: ObjID, f: F) -> R
    where
        F: FnOnce(&mut Self) -> R,
    {
        alloca::alloca(|stack_space| {
            stack_space.write(Self { thread_id, src_ctx });
            // Safety: we init the MaybeUninit just above.
            f(unsafe { stack_space.assume_init_mut() })
        })
    }

    /// Get the ID of the source context, or None if the call was not cross-context.
    pub fn source_context(&self) -> Option<ObjID> {
        if self.src_ctx.as_u128() == 0 {
            None
        } else {
            Some(self.src_ctx)
        }
    }

    /// Get the ID of the calling thread.
    pub fn thread_id(&self) -> ObjID {
        if self.thread_id.as_u128() == 0 {
            twizzler_abi::syscall::sys_thread_self_id()
        } else {
            self.thread_id
        }
    }

    /// Ensures that the data is filled out (may read thread ID from kernel if necessary).
    pub fn canonicalize(self) -> Self {
        Self {
            thread_id: self.thread_id(),
            src_ctx: self.src_ctx,
        }
    }
}

pub fn get_thread_id() -> ObjID {
    twizzler_abi::syscall::sys_thread_self_id()
}

pub fn get_sctx_id() -> ObjID {
    twizzler_abi::syscall::sys_thread_active_sctx_id()
}

pub fn runtime_preentry() {
    extern "C" {
        #[linkage = "extern_weak"]
        fn __twz_rt_cross_compartment_entry();
    }

    unsafe {
        __twz_rt_cross_compartment_entry();
    }
}

pub mod __imp {
    #[linkage = "weak"]
    #[no_mangle]
    pub unsafe extern "C" fn __twz_rt_cross_compartment_entry() {
        core::intrinsics::abort();
    }
}
