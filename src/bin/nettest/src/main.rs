use twizzler::object::ObjID;

fn main() {
    println!("Hello, world!");
    for arg in std::env::args() {
        println!("arg {}", arg);
    }
    let id = std::env::args()
        .nth(1)
        .expect("nettest needs to know net obj id");
    let id = id
        .parse::<u128>()
        .expect(&format!("failed to parse object ID string {}", id));
    let id = ObjID::new(id);
    println!("setup with {:?}", id);
    let o = twizzler_net::client_rendezvous(id);
    println!("ok: {:?}", o);
}
