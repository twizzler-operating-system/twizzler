#![allow(unused_imports)]

extern crate test;
use test::Bencher;

use crate::collections::hachage::PersistentHashMap;
use std::collections::HashMap;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use crate::object::ObjectBuilder;

// bugged
/* 
#[bench]
fn random_insert_500k_volatile(b: &mut Bencher) {
    b.iter(|| {
        let mut phm = PersistentHashMap::with_builder(
            ObjectBuilder::default()
        ).unwrap();

        let mut rng = ChaCha8Rng::seed_from_u64(42);

        for i in 0..500000 {
            phm.insert(rng.gen::<u32>(), i).unwrap();
        }
    });
}
*/

// bugged
/*#[bench]
fn random_insert_500_persistent(b: &mut Bencher) {

    b.iter(|| {
        let mut phm = PersistentHashMap::with_builder(
            ObjectBuilder::default()
        ).unwrap();

        let mut rng = ChaCha8Rng::seed_from_u64(42);

        for i in 0..500 {
            phm.insert(rng.gen::<u32>(), i).unwrap();
        }
    });
}

*/