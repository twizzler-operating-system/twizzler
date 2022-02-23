use scoped_tls_hkt::scoped_thread_local;
use std::{
    cell::Cell,
    task::{Context, Poll},
};

scoped_thread_local! {
    static BUDGET: Cell<u32>
}

pub(crate) fn setup<T>(poll: impl FnOnce() -> T) -> T {
    BUDGET.set(&Cell::new(200), poll)
}

pub(crate) fn poll(cx: &mut Context<'_>) -> Poll<()> {
    if BUDGET.is_set() && BUDGET.with(|b| b.replace(b.get().saturating_sub(1))) == 0 {
        cx.waker().wake_by_ref();
        return Poll::Pending;
    }
    Poll::Ready(())
}
