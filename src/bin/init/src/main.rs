//#![no_std]
#![feature(lang_items)]
#![feature(naked_functions)]
#![feature(thread_local)]
#![feature(duration_constants)]
#![allow(unreachable_code)]
//#![no_main]

/*
#[no_mangle]
pub extern "C" fn std_runtime_starta() {
    twizzler_abi::syscall::sys_kernel_console_write(
        b"hello world\n",
        twizzler_abi::syscall::KernelConsoleWriteFlags::empty(),
    );
    loop {}
}
*/

/*
#[panic_handler]
pub fn __panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}
*/

#[thread_local]
static mut FOO: u32 = 42;
#[thread_local]
static mut BAR: u32 = 0;
#[allow(named_asm_labels)]

static BAZ: AtomicU64 = AtomicU64::new(0);

fn test_thread_sync() {
    let _j = std::thread::spawn(|| {
        let reference = ThreadSyncReference::Virtual(&BAZ as *const AtomicU64);
        let value = 0;
        let wait = ThreadSync::new_sleep(ThreadSyncSleep::new(
            reference,
            value,
            twizzler_abi::syscall::ThreadSyncOp::Equal,
            ThreadSyncFlags::empty(),
        ));

        loop {
            println!("{:?} going to sleep", std::thread::current().id());
            let res = sys_thread_sync(&mut [wait], None);
            println!("woke up: {:?} {:?}", res, wait.get_result());
        }
    });

    let reference = ThreadSyncReference::Virtual(&BAZ as *const AtomicU64);
    let wake = ThreadSync::new_wake(ThreadSyncWake::new(reference, 1));
    let mut c = 0u64;
    loop {
        println!("{:?} waking up {}", std::thread::current().id(), c);
        c += 1;
        let res = sys_thread_sync(&mut [wake], None);
        for _i in 0u64..40000u64 {}
        println!("done {:?}", res);
    }
}

fn test_thread_sync_timeout() {
    let _j = std::thread::spawn(|| {
        let reference = ThreadSyncReference::Virtual(&BAZ as *const AtomicU64);
        let value = 0;
        let wait = ThreadSync::new_sleep(ThreadSyncSleep::new(
            reference,
            value,
            twizzler_abi::syscall::ThreadSyncOp::Equal,
            ThreadSyncFlags::empty(),
        ));

        let mut c = 0u64;
        loop {
            println!("{:?} going to sleep {}", std::thread::current().id(), c);
            let res = sys_thread_sync(&mut [wait], Some(Duration::MILLISECOND * 1000));
            println!("woke up: {:?} {:?}", res, wait.get_result());
            c += 1;
        }
    });

    let reference = ThreadSyncReference::Virtual(&BAZ as *const AtomicU64);
    let _wake = ThreadSync::new_wake(ThreadSyncWake::new(reference, 1));
    let mut _c = 0u64;
    loop {

        // println!("{:?} waking up {}", std::thread::current().id(), c);
        // c += 1;
        // let res = sys_thread_sync(&mut [wake], None);
        // for i in 0u64..40000u64 {}
        // println!("done {:?}", res);
    }
}

struct Foo {
    x: u64,
}

fn test_mutex() {
    let mutex: Arc<Mutex<Foo>> = Arc::new(Mutex::new(Foo { x: 0 }));
    let mutex2 = mutex.clone();
    std::thread::spawn(move || {
        let mut c = 0u64;
        loop {
            let mut data = mutex.lock().unwrap();
            data.x += 1;
            let v = data.x;
            c += 1;
            if c % 1000000 == 0 {
                println!("w {}", data.x);
            }
            assert_eq!(v, data.x);
        }
    });

    let mut c = 0u64;
    loop {
        let mut data = mutex2.lock().unwrap();
        data.x += 1;
        c += 1;
        let v = data.x;
        // for i in 0..1000 {}
        assert_eq!(v, data.x);
        if c % 1000000 == 0 {
            println!("a {}", data.x);
        }
        assert_eq!(v, data.x);
    }
}

fn get_user_input() {
    println!("enter some text:");
    let mut s = String::new();
    std::io::stdin()
        .read_line(&mut s)
        .expect("Did not enter a correct string");
    println!("you typed: {}", s);
}

fn list_subobjs(level: usize, id: ObjID) {
    let mut n = 0;
    loop {
        let res = sys_kaction(
            KactionCmd::Generic(KactionGenericCmd::GetSubObject(
                SubObjectType::Info.into(),
                n,
            )),
            Some(id),
            0,
            0,
            KactionFlags::empty(),
        );
        if res.is_err() {
            break;
        } else if let KactionValue::ObjID(id) = res.unwrap() {
            println!("  sub {:indent$}info {}: {}", "", n, id, indent = level);
        }
        n += 1;
    }

    let mut n = 0;
    loop {
        let res = sys_kaction(
            KactionCmd::Generic(KactionGenericCmd::GetSubObject(
                SubObjectType::Mmio.into(),
                n,
            )),
            Some(id),
            0,
            0,
            KactionFlags::empty(),
        );
        if res.is_err() {
            break;
        } else if let KactionValue::ObjID(id) = res.unwrap() {
            println!("  sub {:indent$}mmio {}: {}", "", n, id, indent = level);
        }
        n += 1;
    }
}

fn enumerate_children(level: usize, id: ObjID) {
    let mut n = 0;
    loop {
        let res = sys_kaction(
            KactionCmd::Generic(KactionGenericCmd::GetChild(n)),
            Some(id),
            0,
            0,
            KactionFlags::empty(),
        );
        if res.is_err() {
            break;
        } else if let KactionValue::ObjID(id) = res.unwrap() {
            println!("{:indent$}{}: {}", "", n, id, indent = level);
            list_subobjs(level, id);
            enumerate_children(level + 4, id);
        }
        n = n + 1;
    }
}

fn test_kaction() {
    let res = sys_kaction(
        KactionCmd::Generic(KactionGenericCmd::GetKsoRoot),
        None,
        0,
        0,
        KactionFlags::empty(),
    );
    println!("{:?}", res);
    let id = match res.unwrap() {
        KactionValue::U64(_) => todo!(),
        KactionValue::ObjID(id) => id,
    };

    enumerate_children(0, id);
}

fn exec(name: &str, id: ObjID, argid: ObjID) {
    let env: Vec<String> = std::env::vars()
        .map(|(n, v)| format!("{}={}", n, v))
        .collect();
    let env_ref: Vec<&[u8]> = env.iter().map(|x| x.as_str().as_bytes()).collect();
    let mut args = vec![name.as_bytes()];
    let argstr = format!("{}", argid.as_u128());
    args.push(argstr.as_bytes());
    let _elf = twizzler_abi::load_elf::spawn_new_executable(id, &args, &env_ref);
    //println!("ELF: {:?}", elf);
}

fn exec2(name: &str, id: ObjID) -> Option<ObjID> {
    let env: Vec<String> = std::env::vars()
        .map(|(n, v)| format!("{}={}", n, v))
        .collect();
    let env_ref: Vec<&[u8]> = env.iter().map(|x| x.as_str().as_bytes()).collect();
    let args = vec![name.as_bytes()];
    twizzler_abi::load_elf::spawn_new_executable(id, &args, &env_ref).ok()
    //println!("ELF: {:?}", elf);
}

fn find_init_name(name: &str) -> Option<ObjID> {
    let init_info = twizzler_abi::aux::get_kernel_init_info();
    for n in init_info.names() {
        if n.name() == name {
            return Some(n.id());
        }
    }
    None
}

fn main() {
    println!("[init] starting userspace");
    let _foo = unsafe { FOO + BAR };
    println!("Hello, World {}", unsafe { FOO + BAR });

    let create = ObjectCreate::new(
        BackingType::Normal,
        LifetimeType::Volatile,
        None,
        ObjectCreateFlags::empty(),
    );
    let netid = twizzler_abi::syscall::sys_object_create(create, &[], &[]).unwrap();
    let devid = twizzler_abi::syscall::sys_object_create(create, &[], &[]).unwrap();
    println!("starting device manager");
    if let Some(id) = find_init_name("devmgr") {
        exec("devmgr", id, devid);
    } else {
        eprintln!("[init] failed to start devmgr");
    }

    println!("waiting for device manager to come up");
    let obj = Object::<std::sync::atomic::AtomicU64>::init_id(
        devid,
        Protections::WRITE | Protections::READ,
        ObjectInitFlags::empty(),
    )
    .unwrap();
    let base = unsafe { obj.base_unchecked() };
    twizzler_abi::syscall::sys_thread_sync(
        &mut [ThreadSync::new_sleep(ThreadSyncSleep::new(
            ThreadSyncReference::Virtual(base),
            0,
            ThreadSyncOp::Equal,
            ThreadSyncFlags::empty(),
        ))],
        None,
    )
    .unwrap();
    println!("device manager is up!");

    println!("starting pager");
    let queue = twizzler_queue::Queue::<RequestFromKernel, CompletionToKernel>::create(
        &CreateSpec::new(LifetimeType::Volatile, BackingType::Normal),
        1024,
        1024,
    )
    .unwrap();

    sys_new_handle(
        queue.object().id(),
        twizzler_abi::syscall::HandleType::PagerQueue,
        NewHandleFlags::empty(),
    )
    .unwrap();
    let queue2 = twizzler_queue::Queue::<RequestFromKernel, CompletionToKernel>::create(
        &CreateSpec::new(LifetimeType::Volatile, BackingType::Normal),
        1024,
        1024,
    )
    .unwrap();
    sys_new_handle(
        queue2.object().id(),
        twizzler_abi::syscall::HandleType::PagerQueue,
        NewHandleFlags::empty(),
    )
    .unwrap();
    if let Some(id) = find_init_name("pager") {
        exec("pager", id, queue.object().id());
    } else {
        eprintln!("[init] failed to start pager");
    }

    std::env::set_var("NETOBJ", format!("{}", netid.as_u128()));
    if let Some(id) = find_init_name("netmgr") {
        exec("netmgr", id, netid);
    } else {
        eprintln!("[init] failed to start netmgr");
    }

    println!("waiting for network manager to come up");
    twizzler_net::wait_until_network_manager_ready(netid);
    println!("network manager is up!");

    if let Some(id) = find_init_name("test_bins") {
        println!("=== found init test list ===");
        let slot = twizzler_abi::slot::global_allocate().unwrap();
        twizzler_abi::syscall::sys_object_map(None, id, slot, Protections::READ, MapFlags::empty())
            .unwrap();

        let addr = twizzler_abi::slot::to_vaddr_range(slot).0;
        let bytes = unsafe {
            core::slice::from_raw_parts(addr as *const u8, twizzler_abi::object::MAX_SIZE)
        };
        let bytes = &bytes[0..bytes.iter().position(|r| *r == 0).unwrap_or(0)];
        let str = String::from_utf8(bytes.to_vec()).unwrap();
        let mut test_failed = false;
        for line in str.split("\n").filter(|l| !l.is_empty()) {
            println!("STARTING TEST {}", line);
            if let Some(id) = find_init_name(line) {
                let tid = exec2(line, id);
                if let Some(tid) = tid {
                    let slot = twizzler_abi::slot::global_allocate().unwrap();
                    twizzler_abi::syscall::sys_object_map(
                        None,
                        tid,
                        slot,
                        Protections::READ,
                        MapFlags::empty(),
                    )
                    .unwrap();
                    let tr = twizzler_abi::slot::to_vaddr_range(slot).0 as *const ThreadRepr;
                    unsafe {
                        let val = tr.as_ref().unwrap().wait(None);
                        if let Some(val) = val {
                            if val != 0 {
                                test_failed = true;
                            }
                        }
                    }
                }
            } else {
                println!("FAILED to start {}", line);
                test_failed = true;
            }
        }
        if test_failed {
            println!("!!! TEST MODE FAILED");
        }
        #[allow(deprecated)]
        twizzler_abi::syscall::sys_debug_shutdown(if test_failed { 1 } else { 0 });
    }

    println!("Hi, welcome to the basic twizzler test console.");
    println!("If you wanted line-editing, you've come to the wrong place.");
    println!("A couple commands you can run:");
    println!("   - 'nt': Run the nettest program");
    println!("... and that's it, but you can add your OWN things with the magic of PROGRAMMING.");
    loop {
        let reply = rprompt::prompt_reply_stdout("> ").unwrap();
        println!("got: <{}>", reply);
        let cmd: Vec<&str> = reply.split(" ").collect();
        if cmd.len() == 2 && cmd[0] == "run" {
            if let Some(id) = find_init_name(cmd[1]) {
                if cmd[1] == "nettest" {
                    exec(cmd[1], id, netid);
                } else {
                    exec(cmd[1], id, ObjID::new(0));
                }
            } else {
                eprintln!("[init] failed to start {}", cmd[1]);
            }
        }

        if cmd.len() == 1 && cmd[0] == "nt" {
            if let Some(id) = find_init_name("nettest") {
                exec("nettest", id, netid);
            } else {
                eprintln!("[init] failed to start nettest");
            }
        }

        //  get_user_input();
    }
    if false {
        test_kaction();
        get_user_input();
        test_thread_sync_timeout();
        test_mutex();
        test_thread_sync();
    }

    loop {}
}

/*
#[naked]
#[no_mangle]
extern "C" fn _start() -> ! {
    unsafe { asm!("call std_runtime_start", options(noreturn)) }
}
*/

use std::{
    sync::{atomic::AtomicU64, Arc, Mutex},
    time::Duration,
};

use twizzler_abi::{
    device::SubObjectType,
    kso::{KactionCmd, KactionFlags, KactionGenericCmd, KactionValue},
    object::{ObjID, Protections},
    pager::{CompletionToKernel, RequestFromKernel},
    syscall::{
        sys_kaction, sys_new_handle, sys_thread_sync, BackingType, LifetimeType, MapFlags,
        NewHandleFlags, ObjectCreate, ObjectCreateFlags, ThreadSync, ThreadSyncFlags, ThreadSyncOp,
        ThreadSyncReference, ThreadSyncSleep, ThreadSyncWake,
    },
    thread::ThreadRepr,
};
use twizzler_object::{CreateSpec, Object, ObjectInitFlags};
