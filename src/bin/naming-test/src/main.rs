use std::fs::File;

use naming::static_naming_factory;
use naming::dynamic_naming_factory;

fn main() {
    let mut handle = dynamic_naming_factory().unwrap();

    println!("Behold the universe: {}", handle.enumerate_names().iter().map(|x| x.0.clone()).collect::<Vec<String>>().join(" "));
    let name = "hello world";
    match handle.get(name) {
        Some(x) => {
            handle.put(name, x - 1);
            println!("{} bottles of beer on the wall. {} bottles of beer! Take one down pass it around you got {} bottles of beer on the wall", x, x, x-1);
        }
        None => {
            handle.put(name, 3);
            println!("No more bottles of beer on the wall, no more bottles of beer! Go to the store and buy some more, {} bottles of beer on the wall...", 3);
        }
    }
}
