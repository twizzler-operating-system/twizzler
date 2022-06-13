use twizzler_abi::object::ObjID;

fn main() {
    let q1id = std::env::var("PAGERQ1OBJ").expect("failed to get kernel request queue ID");
    let q2id = std::env::var("PAGERQ2OBJ").expect("failed to get pager request queue ID");
    let q1id = q1id
        .parse::<u128>()
        .unwrap_or_else(|_| panic!("failed to parse object ID string {}", q1id));
    let q1id = ObjID::new(q1id);
    let q2id = q2id
        .parse::<u128>()
        .unwrap_or_else(|_| panic!("failed to parse object ID string {}", q2id));
    let q2id = ObjID::new(q2id);
    println!("Hello, world from pager! {} {}", q1id, q2id);
}
