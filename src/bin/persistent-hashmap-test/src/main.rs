use naming::GetFlags;
use twizzler::{
    collections::hachage::PersistentHashMap, object::{Object, ObjectBuilder}
};
use std::collections::HashMap;
use twizzler_rt_abi::object::MapFlags;
use std::time::Instant;
use twizzler::marker::Invariant;
use std::fmt::Debug;
use miette::{IntoDiagnostic, Result};

fn open_or_create_hashtable_object<T: Debug + Invariant>(
    name: &str,
) -> Result<PersistentHashMap<u64, T>> {
    let mut nh = naming::dynamic_naming_factory().unwrap();
    let name = format!("/data/ptest-obj-{}", name);
    let vo = if let Ok(node) = nh.get(&name, GetFlags::empty()) {
        println!("reopened: {:?}", node.id);
        let backing = Object::map(node.id, MapFlags::PERSIST | MapFlags::READ | MapFlags::WRITE).into_diagnostic()?;
        let phm = PersistentHashMap::from(backing);

        Ok(phm)
    } else {
        let vo = PersistentHashMap::with_builder(
            ObjectBuilder::default().persist()
        ).unwrap();
        println!("new: {:?}", vo.object().id());
        let _ = nh.remove(&name);
        nh.put(&name, vo.object().id()).into_diagnostic()?;
        Ok(vo)
    };

    vo
}


#[derive(clap::Parser, Clone, Debug)]
struct Cli {
    name: String,
    arg: u64
}


fn performance_test() {
    let mut phm = PersistentHashMap::with_builder(
        ObjectBuilder::default()
    ).unwrap();

    unsafe {phm.resize(16777216)};
    println!("persistent hashmap");
    println!("inserting");
    let now = Instant::now();
    for i in 0..14260633 {
        //println!("inserting {}", i);
        phm.insert(i, i).unwrap();
        
    }
    println!("inserting took {} milli seconds", now.elapsed().as_millis());

    println!("fetching");
    let now = Instant::now();
    for i in 0..14260633 {
        let foo = phm.get(&i).unwrap();
        assert_eq!(&i, foo);
        //println!("val: {} {}", foo.0, foo.1);
    }
    println!("fetching took {} milli seconds", now.elapsed().as_millis());


    println!("regular hashmap");
    let mut hm = HashMap::<u64, u64>::new();
    hm.reserve(16777216);
    println!("inserting");
    let now = Instant::now();

    for i in 0..14260633 {
        //println!("inserting {}", i);
        hm.insert(i, i);
    }
    println!("inserting took {} milli seconds", now.elapsed().as_millis());

    println!("inserted!");
    let now = Instant::now();
    for i in 0..14260633 {
        let foo = hm.get(&i).unwrap();
        assert_eq!(&i, foo);
        //println!("val: {} {}", foo.0, foo.1);
    }
    println!("fetching took {} milli seconds", now.elapsed().as_millis());
}

fn performance_test_2() {
    let mut phm = PersistentHashMap::with_builder(
        ObjectBuilder::default().persist()
    ).unwrap();

    unsafe {phm.resize(16777216)};

    let mut write_session = phm.write_session().unwrap();

    println!("persistent hashmap");
    println!("inserting");
    let now = Instant::now();
    for i in 0..14260633 {
        //println!("inserting {}", i);
        write_session.insert(i, i);
    }

    drop(write_session);
    println!("inserting took {} milli seconds", now.elapsed().as_millis());

    println!("fetching");
    let now = Instant::now();
    for i in 0..14260633 {
        let foo = phm.get(&i).unwrap();
        assert_eq!(&i, foo);
        //println!("val: {} {}", foo.0, foo.1);
    }
    println!("fetching took {} milli seconds", now.elapsed().as_millis());

    println!("regular hashmap");
    let mut hm = HashMap::<u64, u64>::new();
    hm.reserve(16777216);
    println!("inserting");
    let now = Instant::now();

    for i in 0..14260633 {
        //println!("inserting {}", i);
        hm.insert(i, i);
    }
    println!("inserting took {} milli seconds", now.elapsed().as_millis());

    println!("inserted!");
    let now = Instant::now();
    for i in 0..14260633 {
        let foo = hm.get(&i).unwrap();
        assert_eq!(&i, foo);
        //println!("val: {} {}", foo.0, foo.1);
    }
    println!("fetching took {} milli seconds", now.elapsed().as_millis());
}


/*fn performance_test_2() {
    use rand::prelude::*;

    let phm = PersistentHashMap::<[u64; 8], [u8; 128]>::new();
    let mut rng = rand::rng();

    for i in 0..262144 {
        phm.i
    }

    for i in 0..262144 {

    }
}*/

fn correctness_test() {
    let mut phm = PersistentHashMap::with_builder(
        ObjectBuilder::default()
    ).unwrap();

    for i in 0..13 {
        phm.insert(i, i).unwrap();
    }

    for i in 0..13 {
        let val = phm.get(&i).unwrap();
        assert_eq!(val, &i);
    }

    for i in 0..13 {
        assert_eq!(phm.remove(&i).unwrap(), i);
    }

    for i in 0..13 {
        let val = phm.get(&i);
        assert_eq!(val, None);
    }

    for i in 0..13 {
        phm.insert(i, i).unwrap();
    }

    for i in 0..13 {
        let val = phm.get(&i).unwrap();
        assert_eq!(val, &i);
    }
}

fn correctness_test_2() {
    let mut phm = PersistentHashMap::with_builder(
        ObjectBuilder::default()
    ).unwrap();

    println!("hi");
    phm.insert(0, 0).unwrap();
    
    println!("inserted!");
    let val = phm.get(&0).unwrap();

    assert_eq!(&0, val);
    println!("done!");
}

fn main() {
    /*let cli = Cli::parse();

    let mut foo = open_or_create_hashtable_object(&cli.name).unwrap();

    match foo.alter_or_insert(cli.arg, |_, foo| {
        match foo {
            Some(x) => x + 1,
            None => 1,
        }
    }).unwrap() {
        Some(x) => {
            println!("{} has been invoked {} times!", cli.arg, x);
        }
        None => println!("{} has been invoked {} times!", cli.arg, 0)
    }*/

    performance_test_2();
}
