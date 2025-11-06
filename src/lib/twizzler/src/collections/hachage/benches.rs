#![allow(unused_imports)]

extern crate test;
use std::collections::HashMap;

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use test::Bencher;
use twizzler_abi::{
    object::Protections,
    syscall::{
        sys_object_ctrl, BackingType, CreateTieFlags, CreateTieSpec, DeleteFlags, LifetimeType,
        ObjectControlCmd, ObjectCreate, ObjectCreateFlags,
    },
};
use twizzler_rt_abi::object::ObjectCmd;

use crate::{
    collections::hachage::PersistentHashMap,
    object::{Object, ObjectBuilder},
};

#[bench]
fn random_insert_1k_volatile(b: &mut Bencher) {
    let mut phm = PersistentHashMap::with_builder(ObjectBuilder::default()).unwrap();
    b.iter(|| {
        let mut rng = ChaCha8Rng::seed_from_u64(42);

        for i in 0..1000 {
            phm.insert(rng.random::<u32>(), i).unwrap();
        }

        phm.clear().unwrap();
    });

    phm.into_object()
        .into_handle()
        .cmd(ObjectCmd::Delete, 0)
        .unwrap();
}

#[bench]
fn random_std_insert_1k_volatile(b: &mut Bencher) {
    let mut hm = HashMap::new();
    b.iter(|| {
        let mut rng = ChaCha8Rng::seed_from_u64(42);

        for i in 0..1000 {
            hm.insert(rng.random::<u32>(), i);
        }

        hm.clear();
    });
}

#[bench]
fn random_insert_1k_volatile_batch(b: &mut Bencher) {
    let mut phm = PersistentHashMap::with_builder(ObjectBuilder::default()).unwrap();
    b.iter(|| {
        let mut rng = ChaCha8Rng::seed_from_u64(42);

        let mut sesh = phm.write_session().unwrap();
        for i in 0..1000 {
            sesh.insert(rng.random::<u32>(), i).unwrap();
        }

        phm.clear().unwrap();
    });

    phm.into_object()
        .into_handle()
        .cmd(ObjectCmd::Delete, 0)
        .unwrap();
}

#[bench]
fn random_lookup_1k_volatile(b: &mut Bencher) {
    let mut phm = PersistentHashMap::with_builder(ObjectBuilder::default()).unwrap();

    for i in 0..1_000_000u32 {
        phm.insert(i, i).unwrap();
    }

    b.iter(|| {
        let mut rng = ChaCha8Rng::seed_from_u64(42);

        for _ in 0..1_000 {
            let v = phm.get(&(rng.random::<u32>() % 1_000_000)).unwrap();
            std::hint::black_box(v);
        }
    });

    phm.into_object()
        .into_handle()
        .cmd(ObjectCmd::Delete, 0)
        .unwrap();
}

#[bench]
fn random_std_lookup_1k_volatile(b: &mut Bencher) {
    let mut hm = HashMap::new();
    for i in 0..1_000_000u32 {
        hm.insert(i, i);
    }

    b.iter(|| {
        let mut rng = ChaCha8Rng::seed_from_u64(42);

        for _ in 0..1_000 {
            let v = hm.get(&(rng.random::<u32>() % 1_000_000)).unwrap();
            std::hint::black_box(v);
        }
    });
}
// bugged
/*#[bench]
fn random_insert_500_persistent(b: &mut Bencher) {

    b.iter(|| {
        let mut phm = PersistentHashMap::with_builder(
            ObjectBuilder::default()
        ).unwrap();

        let mut rng = ChaCha8Rng::seed_from_u64(42);

        for i in 0..500 {
            phm.insert(rng.random::<u32>(), i).unwrap();
        }
    });
}

*/

#[bench]
fn random_insert_500_persistent_batch(b: &mut Bencher) {
    let mut phm = PersistentHashMap::with_builder(ObjectBuilder::default().persist(true)).unwrap();
    b.iter(|| {
        let mut rng = ChaCha8Rng::seed_from_u64(42);

        let mut sesh = phm.write_session().unwrap();
        for i in 0..100 {
            sesh.insert(rng.random::<u32>(), i).unwrap();
        }
        sys_object_ctrl(phm.object().id(), ObjectControlCmd::Sync).unwrap();
    });
}

#[bench]
fn random_insert_1_persistent(b: &mut Bencher) {
    let mut phm = PersistentHashMap::with_builder(ObjectBuilder::default().persist(true)).unwrap();
    b.iter(|| {
        let mut rng = ChaCha8Rng::seed_from_u64(42);

        let mut sesh = phm.write_session().unwrap();
        sesh.insert(rng.random::<u32>(), 0).unwrap();

        sys_object_ctrl(phm.object().id(), ObjectControlCmd::Sync).unwrap();
    });
}
