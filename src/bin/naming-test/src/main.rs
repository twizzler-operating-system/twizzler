use std::fs::File;

use naming::NamingHandle;

fn main() {
    let mut handle = NamingHandle::new().unwrap();

    match handle.get(&"hello world!") {
        Some(x) => {
            handle.put(&"hello world!", x - 1);
            println!("{} bottles of beer on the wall. {} bottles of beer! Take one down pass it around you got {} bottles of beer on the wall", x, x, x-1);
        }
        None => {
            handle.put(&"hello world!", 99);
            println!("No more bottles of beer on the wall, no more bottles of beer! Go to the store and buy some more, {} bottles of beer on the wall...", 99);
        }
    }
}
