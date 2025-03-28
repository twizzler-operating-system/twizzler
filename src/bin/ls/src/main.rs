use std::{cmp::Ordering, path::PathBuf};

use clap::Parser;
use naming::{static_naming_factory, NsNodeKind, StaticNamingHandle};

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
        if a.kind == NsNodeKind::Namespace {
            Ordering::Greater
        } else if b.kind == NsNodeKind::Namespace {
            Ordering::Less
        } else {
            a.name().cmp(&b.name())
        }
    });

    println!("{}:", foo.display());
    for x in &names {
        foo.push(x.name().unwrap());
        print!("{} ", x.name().unwrap());
        foo.pop();
    }
    println!("\n");
    for x in &names {
        if x.kind != NsNodeKind::Namespace {
            break;
        }
        foo.push(x.name().unwrap());
        handle.change_namespace(&x.name().unwrap()).unwrap();
        recurse(handle, foo);
        handle.change_namespace("..").unwrap();
        foo.pop();
    }
}

fn main() {
    let args = Args::parse();

    use std::fs;

    let paths = fs::read_dir(&args.path.as_ref().unwrap_or(&"/".to_string())).unwrap();
    for path in paths {
        println!("Name: {}", path.unwrap().path().display())
    }

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
            if a.kind == NsNodeKind::Namespace {
                Ordering::Greater
            } else if b.kind == NsNodeKind::Namespace {
                Ordering::Less
            } else {
                a.name().cmp(&b.name())
            }
        });
        for x in &names {
            print!("{} ", x.name().unwrap());
        }
        println!("")
    }
}
