use std::{collections::BTreeMap, time::Duration};

use twizzler::object::TypedObject;
use twizzler_abi::{
    object::ObjID,
    syscall::{EnumerateKind, ThreadSchedStats, sys_thread_read_stats, sys_thread_self_id},
    thread::{ExecutionState, ThreadRepr},
};
use twizzler_rt_abi::{error::TwzError, object::MapFlags};

fn main() {
    let mut tracker = ThreadTracker::default();

    loop {
        tracker.scan_for_threads();
        tracker.read_thread_names();
        tracker.read_thread_stats();

        tracker.show();

        std::thread::sleep(Duration::from_millis(1000));
    }
}

struct ThreadInfo {
    id: ObjID,
    name: Option<String>,
    state: ExecutionState,
    err: Option<TwzError>,
    stats: ThreadSchedStats,
}

impl ThreadInfo {
    fn calc_stats(&self) -> (f64, f64, f64) {
        let total = self.stats.idle + self.stats.system + self.stats.user;
        if total == 0 {
            return (0.0, 0.0, 0.0);
        }
        (
            self.stats.idle as f64 / total as f64,
            self.stats.system as f64 / total as f64,
            self.stats.user as f64 / total as f64,
        )
    }
}

#[derive(Default)]
struct ThreadTracker {
    threads: BTreeMap<ObjID, ThreadInfo>,
}

impl ThreadTracker {
    fn add_thread(&mut self, id: ObjID) {
        let info = ThreadInfo {
            id,
            name: None,
            state: ExecutionState::Running,
            err: None,
            stats: ThreadSchedStats::default(),
        };
        self.threads.insert(id, info);
    }

    fn remove_thread(&mut self, id: &ObjID) {
        self.threads.remove(id);
    }

    fn get_thread_info(&self, id: &ObjID) -> Option<&ThreadInfo> {
        self.threads.get(id)
    }

    fn read_thread_stats(&mut self) {
        for thread_info in self.threads.iter_mut() {
            let handle =
                twizzler::object::Object::<ThreadRepr>::map(*thread_info.0, MapFlags::READ);
            let state = handle.map(|h| h.base().get_state());
            match state {
                Ok(s) => {
                    thread_info.1.state = s;
                }
                Err(e) => {
                    thread_info.1.err = Some(e);
                }
            }
            let stats = sys_thread_read_stats(*thread_info.0, &mut thread_info.1.stats);
            if let Err(e) = stats {
                thread_info.1.err = Some(e);
            }
        }

        self.threads.retain(|_, t| t.err.is_none());
    }

    fn scan_for_threads(&mut self) {
        let mut buf = [ObjID::default(); 128];
        let mut offset = 0;

        loop {
            match twizzler_abi::syscall::sys_enumerate(EnumerateKind::Threads, &mut buf, offset) {
                Ok(count) => {
                    if count == 0 {
                        break;
                    }

                    for i in 0..count {
                        let thread_id = buf[i as usize];

                        if self.get_thread_info(&thread_id).is_none() {
                            self.add_thread(thread_id);
                        }
                    }

                    offset += count;
                }
                Err(e) => {
                    eprintln!("Error enumerating threads: {:?}", e);
                    break;
                }
            }
        }
    }

    fn read_thread_names(&mut self) {
        for thread_info in self.threads.values_mut() {
            if thread_info.name.is_none() {
                // TODO
                thread_info.name = Some(format!("Thread-{}", thread_info.id));
            }
        }
    }

    fn show(&self) {
        println!("Threads (our thread = {:?}):", sys_thread_self_id());
        let mut sleeping = 0;
        for thread_info in self.threads.values() {
            if thread_info.err.is_some() {
                continue;
            }
            if thread_info.state == ExecutionState::Sleeping {
                sleeping += 1;
                continue;
            }
            let name = thread_info.name.as_deref().unwrap_or("<unknown>");
            let (idle, system, user) = thread_info.calc_stats();
            println!(
                "  ID: {:?}, Name: {}, State: {:?}, Stats: idle={:.2}, system={:.2}, user={:.2}",
                thread_info.id, name, thread_info.state, idle, system, user
            );
        }
        println!("Sleeping threads: {}", sleeping);
    }
}
