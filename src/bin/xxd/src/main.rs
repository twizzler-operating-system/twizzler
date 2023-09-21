use fute::file::{File};
use ascii;

use std::io::{Read};

fn foo(ch: char) -> bool {
    if (ch >= '0' && ch <= '9') || 
    (ch >= 'a' && ch <= 'z') || 
    (ch >= 'A' && ch <= 'Z') || ch == '!' 
    || ch == '\"' || ch == '#' || ch == '$' || ch == '%' || ch == '&' || ch == '\'' || ch == '(' || ch == ')' || ch == '*' || ch == '+' || ch == ',' || ch == '-' || ch == '.' || ch == '/' || ch == ':' || ch == ';' || ch == '<' || ch == '=' || ch == '>' || ch == '?' || ch == '@' || ch == '[' || ch == '\\' || ch == ']' || ch == '^' || ch == '`' || ch == '{' || ch == '|' || ch == '}' || ch == ' ' {
        return true;
    }
    else {
        return false;
    }
}

fn bar(ch: char) -> bool {
    if (ch == '\t' || ch == '\n') {
        return true;
    }
    else {
        return false;
    }
}

fn main() {
    let path = std::env::args().nth(1).expect("Path pls");

    let mut f = File::open(&path).expect("Couldn't open file :(");
    let buf: &mut [u8; 16] = &mut [0; 16];
    println!("");

    let mut char_count = 0;
    loop {
        let x = f.read(buf).expect("Can't read file :(");
        if x == 0 {break}

        let entry1 = format!("{:#08X}", char_count);
        let mut entry2 = String::new();
        let mut entry3 = String::new();
        for i in 0..16 {
            if i < x {
                if foo(buf[i] as char) {
                    entry2.push_str("\x1b[0;32m");
                }
                else if bar(buf[i] as char) {
                    entry2.push_str("\x1b[0;33m");
                }
                else {
                    entry2.push_str("\x1b[0;31m");
                }
                entry2.push_str(&format!("{:0>2x}", buf[i]));
                entry2.push_str("\x1b[0m");

            }
            else {
                entry2.push_str("  ");
            }
            if i % 2 == 1 {entry2.push(' ')};
        }

        for i in 0..x {
            let x : char = buf[i] as char;
            if foo(buf[i] as char) {
                entry3.push_str("\x1b[0;32m");
            }
            else if bar(buf[i] as char) {
                entry3.push_str("\x1b[0;33m");
            }
            else {
                entry3.push_str("\x1b[0;31m");
            }

            if foo(x) {
                entry3.push(x); 
            }
            else {
                entry3.push('.');
            }
            entry3.push_str("\x1b[0m");
        }

        println!("{}: {} {}", entry1, entry2, entry3);
        char_count += x;

    }
}
