#![feature(linkage)]
#![feature(native_link_modifiers_as_needed)]

use std::sync::atomic::{AtomicBool, Ordering};

extern crate montest_lib;
extern crate secgate;

secgate::secgate_prelude!();

#[link(name = "montest_lib", kind = "dylib", modifiers = "-as-needed")]
extern "C" {}

extern crate tracing;
extern crate tracing_subscriber;
extern crate twizzler_runtime;

fn main() {
    setup_logging();
    montest_lib::test_global_call_count();
}
use tracing::Level;
fn setup_logging() {
    let _ = tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_max_level(Level::DEBUG)
            .finish(),
    );
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use monitor_api::CompartmentHandle;
    use twizzler_abi::klog_println;

    use crate::montest_lib;
    extern crate secgate;

    use super::setup_logging;
    use crate::WAS_CTOR_RUN;

    #[test]
    fn test_tl_count() {
        setup_logging();
        assert_eq!(
            secgate::SecGateReturn::Success(1),
            montest_lib::test_thread_local_call_count()
        );
    }

    #[test]
    fn test_gl_count() {
        setup_logging();
        assert_eq!(
            secgate::SecGateReturn::Success(1),
            montest_lib::test_global_call_count()
        );
    }

    #[test]
    fn test_uncaught_internal_panic() {
        setup_logging();
        assert_eq!(
            secgate::SecGateReturn::CalleePanic,
            montest_lib::test_internal_panic(false)
        );
    }

    #[test]
    fn test_internal_panic() {
        setup_logging();
        assert_eq!(
            secgate::SecGateReturn::Success(1),
            montest_lib::test_internal_panic(true)
        );
    }

    #[test]
    fn test_lib_ctors() {
        setup_logging();
        assert_eq!(
            secgate::SecGateReturn::Success(true),
            montest_lib::test_was_ctor_run()
        );
    }

    #[test]
    fn test_bin_ctors() {
        setup_logging();
        assert_eq!(true, WAS_CTOR_RUN.load(Ordering::SeqCst))
    }

    #[test]
    fn test_dynamic_secgate() {
        let current = CompartmentHandle::current();
        let name = format!("{}::libmontest_lib.so", current.info().name);
        let comp = CompartmentHandle::lookup(&name)
            .expect(&format!("failed to open compartment: {}", &name));
        let gate = unsafe { comp.dynamic_gate::<(u32,), u32>("dynamic_test") }.unwrap();
        let ret = unsafe { secgate::dynamic_gate_call(gate, (3,)).ok().unwrap() };
        assert_eq!(ret, 45);
    }
}

static WAS_CTOR_RUN: AtomicBool = AtomicBool::new(false);

#[used]
#[doc(hidden)]
#[allow(non_upper_case_globals)]
#[link_section = ".init_array"]
static ___cons_test___ctor: unsafe extern "C" fn() = {
    #[allow(non_snake_case)]
    #[link_section = ".text.startup"]
    unsafe extern "C" fn ___cons_test___ctor() {
        cons_test()
    }
    ___cons_test___ctor
};
unsafe fn cons_test() {
    WAS_CTOR_RUN.store(true, Ordering::SeqCst);
}
