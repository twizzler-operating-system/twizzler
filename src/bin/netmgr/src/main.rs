use twizzler::object::ObjID;

fn main() {
    println!("Hello, world from netmgr!");
    for arg in std::env::args() {
        println!("arg {}", arg);
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
