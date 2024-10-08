use std::sync::atomic::{AtomicBool, Ordering};

extern crate monitor;
extern crate montest_lib;
extern crate twz_rt;

secgate::secgate_prelude!();

fn main() {
    montest_lib::test_global_call_count();
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use crate::WAS_CTOR_RUN;

    #[test]
    fn test_tl_count() {
        assert_eq!(
            secgate::SecGateReturn::Success(1),
            montest_lib::test_thread_local_call_count()
        );
    }

    #[test]
    fn test_gl_count() {
        assert_eq!(
            secgate::SecGateReturn::Success(1),
            montest_lib::test_global_call_count()
        );
    }

    #[test]
    fn test_internal_panic() {
        assert_eq!(
            secgate::SecGateReturn::CalleePanic,
            montest_lib::test_internal_panic()
        );
    }

    #[test]
    fn test_lib_ctors() {
        assert_eq!(
            secgate::SecGateReturn::Success(true),
            montest_lib::test_was_ctor_run()
        );
    }

    #[test]
    fn test_bin_ctors() {
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
