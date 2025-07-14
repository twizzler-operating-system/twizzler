use naming::GetFlags;
use twizzler::{
    collections::hachage::PersistentHashMap,
    object::{Object, ObjectBuilder},
};
use std::{collections::HashMap, hash::Hash};
use twizzler_rt_abi::object::MapFlags;
use std::time::Instant;

fn u8_extend() {
    todo!()
}

fn main() {
    let mut phm = PersistentHashMap::<u64, u64>::new().unwrap();
    unsafe { phm.resize(1048576) };

    println!("persistent hashmap");
    println!("inserting");
    let now = Instant::now();
    for i in 0..90000 {
        //println!("inserting {}", i);
        phm.insert(i, i).unwrap();
    }
    println!("inserting took {} milli seconds", now.elapsed().as_millis());
    
    println!("fetching");
    let now = Instant::now();
    for i in 0..90000 {
        let foo = phm.get(&i).unwrap();
        assert_eq!(&i, foo);
        //println!("val: {} {}", foo.0, foo.1);
    }
    println!("fetching took {} milli seconds", now.elapsed().as_millis());


    println!("regular hashmap");
    let mut hm = HashMap::<u64, u64>::with_capacity(1048576);

    println!("inserting");
    let now = Instant::now();

    for i in 0..90000 {
        //println!("inserting {}", i);
        hm.insert(i, i);
    }
    println!("inserting took {} milli seconds", now.elapsed().as_millis());

    println!("inserted!");
    let now = Instant::now();
    for i in 0..90000 {
        let foo = hm.get(&i).unwrap();
        assert_eq!(&i, foo);
        //println!("val: {} {}", foo.0, foo.1);
    }
    println!("fetching took {} milli seconds", now.elapsed().as_millis());
}