#![feature(thread_local)]

use twizzler::object::ObjID;

async fn get7() -> i32 {
    println!("hello from async");
    4 + 3
}

#[thread_local]
static AAA: [u8; 15] = [0; 15];

fn test_async() {
    println!(
        "main thread id {:?} {}",
        std::thread::current().id(),
        AAA[1]
    );
    let res = twizzler_async::block_on(get7());
    println!("async_block: {}", res);

    let res = twizzler_async::run(get7());
    println!("async_run: {}", res);

    let num_threads = 1;
    for _ in 0..num_threads {
        std::thread::spawn(|| twizzler_async::run(std::future::pending::<()>()));
    }

    let res = twizzler_async::block_on(async {
        let x = twizzler_async::Task::spawn(async {
            println!("hello from task thread {:?}", std::thread::current().id());
            let x = get7().await;
            println!("here");
            x
        })
        .await;
        println!("here2");
        x
    });
    println!("async_thread_pool: {}", res);
}

fn main() {
    println!("Hello, world from netmgr!");
    for arg in std::env::args() {
        println!("arg {}", arg);
    }
    if std::env::args().len() < 10 {
        test_async();
    }
    for _ in 0..4 {
        std::thread::spawn(|| println!("hello from thread {:?}", std::thread::current().id()));
    }
    let id = std::env::args()
        .nth(1)
        .expect("netmgr needs to know net obj id");
    let id = id
        .parse::<u128>()
        .expect(&format!("failed to parse object ID string {}", id));
    let id = ObjID::new(id);
    println!("setup with {:?}", id);

    loop {
        println!("[netmgr] waiting");
        let o = twizzler_net::server_rendezvous(id);
        println!("[netmgr] got {:?}", o);
    }
}
