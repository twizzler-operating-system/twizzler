
// Filesystem in Twizzler Space

use std::{str::from_utf8, env, io::BufRead};
use fute::shell;
fn main() {
    for (n,v) in env::vars() {
        println!("{}: {}", n,v);
    }
}