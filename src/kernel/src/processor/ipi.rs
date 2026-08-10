use alloc::{boxed::Box, sync::Arc};
use core::sync::atomic::{AtomicU64, Ordering};

use super::{
    mp::{all_processors, current_processor, get_processor},
    spin_wait_until,
};
use crate::{
    arch::{self, interrupt::GENERIC_IPI_VECTOR},
    interrupt::{self, Destination},
    thread::current_thread_ref,
};

pub struct IpiTask {
    pub(super) outstanding: AtomicU64,
    pub(super) func: Box<dyn Fn() + Sync + Send>,
}

/// Pauses (one per 100 spins in `spin_wait_until`) before a waiting sender says so. Large enough
/// that a merely-busy target never trips it, small enough to land well inside a run's budget.
const IPI_STALL_PAUSES: usize = 100_000;

fn enqueue_ipi_task_many(incl_self: bool, task: &Arc<IpiTask>) {
    let current = current_processor();
    for p in all_processors().iter().flatten() {
        if p.id != current.id || incl_self {
            p.enqueue_ipi_task(task.clone());
        }
    }
}

/// Run a closure on some set of CPUs.
///
/// With `wait`, this blocks until every target has run the closure. Without it the closure is
/// queued and poked for, and the caller returns immediately -- so it must not depend on the
/// closure having run.
///
/// Not waiting is what a caller wants when the IPI is a nudge rather than a handshake, because a
/// waiting sender can be blocked indefinitely: `spin_wait_iteration` drains TLB shootdowns but not
/// IPI tasks, so a target spinning on a lock with interrupts masked never acknowledges. That is a
/// cycle whenever the sender holds something the target is spinning for.
pub fn ipi_exec(target: Destination, f: Box<dyn Fn() + Send + Sync>, wait: bool) {
    if current_thread_ref().is_none() {
        return;
    }
    let task = Arc::new(IpiTask {
        outstanding: AtomicU64::new(0),
        func: f,
    });

    // We need to disable interrupts to prevent our current CPU from changing until we've submitted
    // the IPIs.
    let int_state = interrupt::disable();
    let current = current_processor();
    match target {
        // Lowest priority doesn't really make sense in IPIs, so we just pretend it goes to BSP.
        Destination::Bsp | Destination::LowestPriority => {
            if current.is_bsp() {
                // We are the only recipients, so just run the closure.
                (task.func)();
                interrupt::set(int_state);
                return;
            }
            get_processor(current.bsp_id()).enqueue_ipi_task(task.clone());
        }
        Destination::Single(id) => {
            let proc = get_processor(id);
            if !proc.is_running() {
                logln!("tried to send IPI to non-running CPU");
                interrupt::set(int_state);
                return;
            }
            if proc.id == current.id {
                // We are the only recipients, so just run the closure.
                (task.func)();
                interrupt::set(int_state);
                return;
            }
            proc.enqueue_ipi_task(task.clone());
        }
        Destination::AllButSelf => enqueue_ipi_task_many(false, &task),
        Destination::All => enqueue_ipi_task_many(true, &task),
    }

    // No point using the IPI hardware to send ourselves a message, so just run it manually if
    // current CPU is included.
    let (target, target_self) = match target {
        Destination::All => (Destination::AllButSelf, true),
        x => (x, false),
    };
    arch::send_ipi(target, GENERIC_IPI_VECTOR);

    if target_self {
        current.run_ipi_tasks();
    }

    // We can take interrupts while we wait for other CPUs to execute.
    interrupt::set(int_state);

    // The queued `Arc`s keep the task alive past our return, so a non-waiting caller needs nothing
    // more than the poke above.
    if wait {
        // This wait had no diagnostic of any kind, so a cpu stuck here left nothing in the
        // transcript -- only a dump showing it running in the kernel with no lock to blame.
        // Reported once rather than bounded: a missing ack is not something to paper over, and
        // the count says how many targets are deaf.
        let mut pauses = 0usize;
        spin_wait_until(
            || {
                if task.outstanding.load(Ordering::SeqCst) != 0 {
                    None
                } else {
                    Some(())
                }
            },
            || {
                pauses += 1;
                if pauses == IPI_STALL_PAUSES {
                    emerglogln!(
                        "ipi stall: cpu {} waiting on {} unacked target(s) after {} pauses",
                        current.id,
                        task.outstanding.load(Ordering::SeqCst),
                        pauses,
                    );
                }
                if !int_state {
                    current.run_ipi_tasks();
                }
            },
        );
    }

    core::sync::atomic::fence(Ordering::SeqCst);
}

pub fn generic_ipi_handler() {
    let current = current_processor();
    current.run_ipi_tasks();
    core::sync::atomic::fence(Ordering::SeqCst);
}

#[cfg(test)]
mod test {
    use alloc::{boxed::Box, sync::Arc};
    use core::sync::atomic::{AtomicUsize, Ordering};

    use twizzler_kernel_macros::kernel_test;

    use crate::{interrupt::Destination, processor::mp::all_processors};

    const NR_IPI_TEST_ITERS: usize = 1000;
    #[kernel_test]
    fn ipi_test() {
        for _ in 0..NR_IPI_TEST_ITERS {
            let nr_cpus = all_processors().iter().flatten().count();
            let counter = Arc::new(AtomicUsize::new(0));
            let counter2 = counter.clone();
            super::ipi_exec(
                Destination::All,
                Box::new(move || {
                    counter2.fetch_add(1, Ordering::SeqCst);
                }),
                true,
            );
            assert_eq!(nr_cpus, counter.load(Ordering::SeqCst));

            let counter = Arc::new(AtomicUsize::new(0));
            let counter2 = counter.clone();
            super::ipi_exec(
                Destination::AllButSelf,
                Box::new(move || {
                    counter2.fetch_add(1, Ordering::SeqCst);
                }),
                true,
            );
            assert_eq!(nr_cpus, counter.load(Ordering::SeqCst) + 1);

            let counter = Arc::new(AtomicUsize::new(0));
            let counter2 = counter.clone();
            super::ipi_exec(
                Destination::Bsp,
                Box::new(move || {
                    counter2.fetch_add(1, Ordering::SeqCst);
                }),
                true,
            );
            assert_eq!(1, counter.load(Ordering::SeqCst));

            let counter = Arc::new(AtomicUsize::new(0));
            let counter2 = counter.clone();
            super::ipi_exec(
                Destination::Single(0),
                Box::new(move || {
                    counter2.fetch_add(1, Ordering::SeqCst);
                }),
                true,
            );
            assert_eq!(1, counter.load(Ordering::SeqCst));

            let counter = Arc::new(AtomicUsize::new(0));
            let counter2 = counter.clone();
            super::ipi_exec(
                Destination::LowestPriority,
                Box::new(move || {
                    counter2.fetch_add(1, Ordering::SeqCst);
                }),
                true,
            );
            assert_eq!(1, counter.load(Ordering::SeqCst));
        }
    }

    /// A non-waiting send still reaches every cpu; the caller just cannot assume it on return.
    /// Bounded rather than spun on forever, so a regression fails with a message instead of
    /// wedging the test thread.
    #[kernel_test]
    fn ipi_test_nowait() {
        const SETTLE_ITERS: usize = 100_000_000;
        let nr_cpus = all_processors().iter().flatten().count();
        let counter = Arc::new(AtomicUsize::new(0));
        let counter2 = counter.clone();
        super::ipi_exec(
            Destination::All,
            Box::new(move || {
                counter2.fetch_add(1, Ordering::SeqCst);
            }),
            false,
        );

        let mut iters = 0;
        while counter.load(Ordering::SeqCst) < nr_cpus && iters < SETTLE_ITERS {
            iters += 1;
            core::hint::spin_loop();
        }
        assert_eq!(nr_cpus, counter.load(Ordering::SeqCst));
    }
}
