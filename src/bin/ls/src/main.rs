use std::{cmp::Ordering, path::PathBuf};

use clap::Parser;
use naming::{static_naming_factory, EntryType, StaticNamingHandle};

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    #[arg(short, long)]
    recursive: bool,
    path: Option<String>,
}

fn recurse(handle: &mut StaticNamingHandle, foo: &mut PathBuf) {
    let mut names = handle.enumerate_names().unwrap();
    names.sort_by(|a, b| {
        if a.entry_type == EntryType::Namespace {
            Ordering::Greater
        } else if b.entry_type == EntryType::Namespace {
            Ordering::Less
        } else {
            a.name.cmp(&b.name)
        }
    });

    println!("{}:", foo.display());
    for x in &names {
        foo.push(x.name);
        print!("{} ", x.name);
        foo.pop();
    }
    println!("\n");
    for x in &names {
        if x.entry_type != EntryType::Namespace {
            break;
        }
        foo.push(x.name);
        handle.change_namespace(&x.name).unwrap();
        recurse(handle, foo);
        handle.change_namespace("..").unwrap();
        foo.pop();
    }
}

fn main() {
    let args = Args::parse();

    println!("Zx");
    let mut namer = static_naming_factory().unwrap();

    if args.recursive {
        let mut path = PathBuf::new();
        path.push(".");
        recurse(&mut namer, &mut path);
    } else {
        let mut names = namer
            .enumerate_names_relative(&args.path.unwrap_or("/".to_string()))
            .unwrap();
        names.sort_by(|a, b| {
            if a.entry_type == EntryType::Namespace {
                Ordering::Greater
            } else if b.entry_type == EntryType::Namespace {
                Ordering::Less
            } else {
                a.name.cmp(&b.name)
            }
        });
        for x in &names {
            print!("{} ", x.name);
        }
        println!("")
    }
}
