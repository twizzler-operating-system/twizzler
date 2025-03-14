use std::{fs::File, io::Read};

fn main() {
    for arg in std::env::args().skip(1) {
        let mut file = File::open(arg).unwrap();
        let mut s = String::new();
        file.read_to_string(&mut s).unwrap();
        print!("{}", s);
    }
}
