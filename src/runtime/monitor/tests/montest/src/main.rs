use std::sync::atomic::{AtomicBool, Ordering};

extern crate secgate;

secgate::secgate_prelude!();

extern crate tracing;
extern crate tracing_subscriber;
extern crate twizzler_runtime;

mod montest_lib {
    #[link(name = "montest_lib")]
    extern "C" {}
    #[secgate::secure_gate(options(info, api))]
    pub fn test_was_ctor_run(info: &GateCallInfo) -> bool {}

    #[secgate::secure_gate(options(info, api))]
    pub fn test_internal_panic(info: &GateCallInfo, catch_it: bool) -> usize {}

    #[secgate::secure_gate(options(info, api))]
    pub fn test_global_call_count(info: &GateCallInfo) -> usize {}

    #[secgate::secure_gate(options(info, api))]
    pub fn test_thread_local_call_count(info: &GateCallInfo) -> usize {}
}

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
