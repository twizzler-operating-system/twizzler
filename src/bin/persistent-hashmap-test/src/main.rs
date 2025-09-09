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
use clap::Parser;
use rand::{rng, Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

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
    arg: u64,
}


fn performance_test() {
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
    for i in 0u64..14260633u64 {
        let foo = phm.get(&i).unwrap();
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

fn performance_test_3() {
    let mut phm = PersistentHashMap::with_builder(
        ObjectBuilder::default().persist()
    ).unwrap();

    unsafe {phm.resize(10000)};


    println!("persistent hashmap");
    println!("inserting");
    let now = Instant::now();
    for i in 0..10000 {
        phm.insert(i, i);
        //println!("inserting {}", i);
    }

    println!("inserting took {} milli seconds", now.elapsed().as_millis());

    println!("fetching");
    let now = Instant::now();
    for i in 0..(100000 * 10000) {
        let foo = phm.get(&(i % 1000)).unwrap();
        assert_eq!(&(i % 1000), foo);
        //println!("val: {} {}", foo.0, foo.1);
    }
    println!("fetching took {} milli seconds", now.elapsed().as_millis());
}

fn performance_test_4() {
    let random_seed: u64 = rng().random();
    let mut rng = ChaCha8Rng::seed_from_u64(random_seed);
    let mut bytes = [0u8; 256];

    println!("regular hashmap");
    let mut hm = HashMap::<[u8; 256], u64>::new();
    println!("inserting");
    let now = Instant::now();

    for i in 0..1426000 {
        rng.fill(&mut bytes);

        //println!("inserting {}", i);
        hm.insert(bytes.clone(), i);
    }
    println!("inserting took {} milli seconds", now.elapsed().as_millis());

    println!("inserted!");
    let mut rng = ChaCha8Rng::seed_from_u64(random_seed);

    let now = Instant::now();
    for i in 0..1426000 {
        rng.fill(&mut bytes);

        let foo = hm.get(&bytes).unwrap();
        assert_eq!(foo, &i)
        //println!("val: {} {}", foo.0, foo.1);
    }
    println!("fetching took {} milli seconds", now.elapsed().as_millis());

    let mut phm = PersistentHashMap::with_builder(
        ObjectBuilder::default()
    ).unwrap();

    let mut rng = ChaCha8Rng::seed_from_u64(random_seed);

    println!("persistent hashmap");
    println!("inserting");
    let now = Instant::now();
    for i in 0..1426000 {
        rng.fill(&mut bytes);

        //println!("inserting {}", i);
        phm.insert(bytes.clone(), i).unwrap();
        
    }
    println!("inserting took {} milli seconds", now.elapsed().as_millis());
    let mut rng = ChaCha8Rng::seed_from_u64(random_seed);

    println!("fetching");
    let now = Instant::now();
    for i in 0..1426000u64 {
        rng.fill(&mut bytes);

        let foo = phm.get(&bytes).unwrap();
        assert_eq!(&i, foo);
        //println!("val: {} {}", foo.0, foo.1);
    }
    println!("fetching took {} milli seconds", now.elapsed().as_millis());
    
}

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

fn correctness_test_3() {
    let mut phm = PersistentHashMap::with_builder(
        ObjectBuilder::default()
    ).unwrap();

    for i in 0..13 {
        phm.insert(i, i).unwrap();
    }

    for mut val in phm.values_mut().unwrap() {
        *val += 1;
    }

    let mut i = 0;
    for (key, val) in phm.iter() {
        println!("{} {}", key, val);
        assert_eq!(&(key + 1), val);
        i+=1;
    }

    assert_eq!(i, 13);
}

fn main() {
    /*let cli = Cli::parse();

    let mut foo = open_or_create_hashtable_object::<u64>("phm").unwrap();

    let mut write_sesh = foo.write_session().unwrap();

    let bar = write_sesh.get_mut(&cli.arg);

    match bar {
        Some(x) => {*x = *x + 1; println!("x now {}", x);}
        None => {println!("new val! x = 1"); write_sesh.insert(cli.arg, 1);}
    }

    drop(write_sesh);*/


    correctness_test_3();
}
