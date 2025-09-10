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

fn enqueue_ipi_task_many(incl_self: bool, task: &Arc<IpiTask>) {
    let current = current_processor();
    for p in all_processors().iter().flatten() {
        if p.id != current.id || incl_self {
            p.enqueue_ipi_task(task.clone());
        }
    }
}

/// Run a closure on some set of CPUs, waiting for all invocations to complete.
pub fn ipi_exec(target: Destination, f: Box<dyn Fn() + Send + Sync>) {
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

    spin_wait_until(
        || {
            if task.outstanding.load(Ordering::SeqCst) != 0 {
                None
            } else {
                Some(())
            }
        },
        || {
            if !int_state {
                current.run_ipi_tasks();
            }
        },
    );

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
            );
            assert_eq!(nr_cpus, counter.load(Ordering::SeqCst));

            let counter = Arc::new(AtomicUsize::new(0));
            let counter2 = counter.clone();
            super::ipi_exec(
                Destination::AllButSelf,
                Box::new(move || {
                    counter2.fetch_add(1, Ordering::SeqCst);
                }),
            );
            assert_eq!(nr_cpus, counter.load(Ordering::SeqCst) + 1);

            let counter = Arc::new(AtomicUsize::new(0));
            let counter2 = counter.clone();
            super::ipi_exec(
                Destination::Bsp,
                Box::new(move || {
                    counter2.fetch_add(1, Ordering::SeqCst);
                }),
            );
            assert_eq!(1, counter.load(Ordering::SeqCst));

            let counter = Arc::new(AtomicUsize::new(0));
            let counter2 = counter.clone();
            super::ipi_exec(
                Destination::Single(0),
                Box::new(move || {
                    counter2.fetch_add(1, Ordering::SeqCst);
                }),
            );
            assert_eq!(1, counter.load(Ordering::SeqCst));

            let counter = Arc::new(AtomicUsize::new(0));
            let counter2 = counter.clone();
            super::ipi_exec(
                Destination::LowestPriority,
                Box::new(move || {
                    counter2.fetch_add(1, Ordering::SeqCst);
                }),
            );
            assert_eq!(1, counter.load(Ordering::SeqCst));
        }
    }
}
