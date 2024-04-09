//#![no_std]
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
    let _elf = twizzler_abi::runtime::load_elf::spawn_new_executable(id, &args, &env_ref);
    //println!("ELF: {:?}", elf);
}

fn exec2(name: &str, id: ObjID) -> Option<ObjID> {
    let env: Vec<String> = std::env::vars()
        .map(|(n, v)| format!("{}={}", n, v))
        .collect();
    let env_ref: Vec<&[u8]> = env.iter().map(|x| x.as_str().as_bytes()).collect();
    let args = vec![name.as_bytes()];
    twizzler_abi::runtime::load_elf::spawn_new_executable(id, &args, &env_ref).ok()
    //println!("ELF: {:?}", elf);
}

fn exec_n(name: &str, id: ObjID, args: &[&str]) {
    let env: Vec<String> = std::env::vars()
        .map(|(n, v)| format!("{}={}", n, v))
        .collect();
    let env_ref: Vec<&[u8]> = env.iter().map(|x| x.as_str().as_bytes()).collect();
    let mut fullargs = vec![name.as_bytes()];
    fullargs.extend(args.iter().map(|x| x.as_bytes()));
    let _elf = twizzler_abi::runtime::load_elf::spawn_new_executable(id, &fullargs, &env_ref);
    //println!("ELF: {:?}", elf);
}

fn find_init_name(name: &str) -> Option<ObjID> {
    let init_info = twizzler_abi::runtime::get_kernel_init_info();
    for n in init_info.names() {
        if n.name() == name {
            return Some(n.id());
        }
    }
    None
}

#[derive(Default)]
struct IoTest {
    switch: AtomicU64,
    _pin: PhantomPinned,
}

impl TwizzlerWaitable for IoTest {
    fn wait_item_read(&self) -> twizzler_abi::syscall::ThreadSyncSleep {
        ThreadSyncSleep::new(
            ThreadSyncReference::Virtual(&self.switch),
            0,
            ThreadSyncOp::Equal,
            ThreadSyncFlags::empty(),
        )
    }

    fn wait_item_write(&self) -> twizzler_abi::syscall::ThreadSyncSleep {
        ThreadSyncSleep::new(
            ThreadSyncReference::Virtual(&self.switch),
            1,
            ThreadSyncOp::Equal,
            ThreadSyncFlags::empty(),
        )
    }
}

impl IoTest {
    fn try_read(&self) -> io::Result<()> {
        match self
            .switch
            .compare_exchange(1, 0, Ordering::SeqCst, Ordering::SeqCst)
        {
            Ok(_) => {
                let _ = twizzler_abi::syscall::sys_thread_sync(
                    &mut [ThreadSync::new_wake(ThreadSyncWake::new(
                        ThreadSyncReference::Virtual(&self.switch),
                        usize::MAX,
                    ))],
                    None,
                );
                Ok(())
            }
            Err(_) => Err(ErrorKind::WouldBlock.into()),
        }
    }

    fn try_write(&self) -> io::Result<()> {
        match self
            .switch
            .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
        {
            Ok(_) => {
                let _ = twizzler_abi::syscall::sys_thread_sync(
                    &mut [ThreadSync::new_wake(ThreadSyncWake::new(
                        ThreadSyncReference::Virtual(&self.switch),
                        usize::MAX,
                    ))],
                    None,
                );
                Ok(())
            }
            Err(_) => Err(ErrorKind::WouldBlock.into()),
        }
    }
}

type AsyncIoTest = Async<IoTest>;

async fn async_test_async() {
    let mut timer1 = async_io::Timer::interval(Duration::from_millis(123));
    let mut timer2 = async_io::Timer::interval(Duration::from_millis(456));

    let ait = Async::new(IoTest::default()).unwrap();
    let ait2 = Arc::new(Async::new(IoTest::default()).unwrap());
    let ait2_clone = ait2.clone();

    let fr = async {
        loop {
            let r = ait.read_with(|io| io.try_read()).await;
            println!("read! {:?}", r);
            let timer1 = async_io::Timer::interval(Duration::from_millis(73));
            timer1.await;
        }
    };

    let fr2 = async {
        loop {
            let r = ait2.read_with(|io| io.try_read()).await;
            println!("extern read! {:?}", r);
        }
    };

    std::thread::spawn(move || loop {
        println!("external write");
        ait2_clone.get_ref().try_write();
        std::thread::sleep(Duration::from_millis(307));
    });

    let fw = async {
        loop {
            let w = ait.write_with(|io| io.try_write()).await;
            println!("write! {:?}", w);
            let timer1 = async_io::Timer::interval(Duration::from_millis(211));
            timer1.await;
        }
    };
    let mut fr = std::pin::pin!(FutureExt::fuse(fr));
    let mut fr2 = std::pin::pin!(FutureExt::fuse(fr2));
    let mut fw = std::pin::pin!(FutureExt::fuse(fw));

    loop {
        let mut timer1 = FutureExt::fuse(timer1.next());
        let mut timer2 = FutureExt::fuse(timer2.next());
        println!("loop");
        futures::select! {
            _ = timer1 => println!("timer1"),
            _ = timer2 => println!("timer2"),
            _ = fr => println!("fr"),
            _ = fr2 => println!("fr2"),
            _ = fw => println!("fw"),
        }
    }
}

fn test_async() {
    let e = async_executor::Executor::new();
    e.spawn(async {
        println!("hello!");
        async_test_async().await
    })
    .detach();
    block_on(e.run(std::future::pending::<()>()));
}

fn main() {
    println!("[init] starting userspace");
    let _foo = unsafe { FOO + BAR };
    println!("Hello, World {}", unsafe { FOO + BAR });

    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .finish(),
    )
    .unwrap();
    test_async();
    loop {}

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
    const DEFAULT_PAGER_QUEUE_LEN: usize = 1024;
    let queue = twizzler_queue::Queue::<RequestFromKernel, CompletionToKernel>::create(
        &CreateSpec::new(LifetimeType::Volatile, BackingType::Normal),
        DEFAULT_PAGER_QUEUE_LEN,
        DEFAULT_PAGER_QUEUE_LEN,
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
        DEFAULT_PAGER_QUEUE_LEN,
        DEFAULT_PAGER_QUEUE_LEN,
    )
    .unwrap();
    sys_new_handle(
        queue2.object().id(),
        twizzler_abi::syscall::HandleType::PagerQueue,
        NewHandleFlags::empty(),
    )
    .unwrap();
    if let Some(id) = find_init_name("pager") {
        exec_n(
            "pager",
            id,
            &[
                &queue.object().id().as_u128().to_string(),
                &queue2.object().id().as_u128().to_string(),
            ],
        );
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
        let runtime = __twz_get_runtime();
        let handle = runtime
            .map_object(id.as_u128(), Protections::READ.into())
            .unwrap();

        let addr = unsafe { handle.start.add(NULLPAGE_SIZE) };
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
                    let thandle = runtime
                        .map_object(tid.as_u128(), Protections::READ.into())
                        .unwrap();

                    let taddr = unsafe { thandle.start.add(NULLPAGE_SIZE) };
                    let tr = taddr as *const ThreadRepr;
                    unsafe {
                        let val = tr.as_ref().unwrap().wait(None);
                        if let Some(val) = val {
                            if val.0 == ExecutionState::Exited && val.1 != 0
                                || val.0 == ExecutionState::Suspended
                            {
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
    future,
    io::{self, ErrorKind},
    marker::PhantomPinned,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use async_io::{block_on, Async};
use futures::{FutureExt, StreamExt};
use polling::BorrowedTwizzlerWaitable;
use twizzler_abi::{
    device::SubObjectType,
    kso::{KactionCmd, KactionFlags, KactionGenericCmd, KactionValue},
    object::{ObjID, Protections, NULLPAGE_SIZE},
    pager::{CompletionToKernel, RequestFromKernel},
    runtime::__twz_get_runtime,
    //thread::{ExecutionState, ThreadRepr},
    syscall::{
        sys_kaction,
        sys_new_handle,
        sys_thread_sync,
        BackingType,
        LifetimeType, //MapFlags,
        NewHandleFlags,
        ObjectCreate,
        ObjectCreateFlags,
        ThreadSync,
        ThreadSyncFlags,
        ThreadSyncOp,
        ThreadSyncReference,
        ThreadSyncSleep,
        ThreadSyncWake,
    },
    thread::{ExecutionState, ThreadRepr},
};
use twizzler_futures::TwizzlerWaitable;
use twizzler_object::{CreateSpec, Object, ObjectInitFlags};
