use naming::GetFlags;
use twizzler::{
    collections::{vec::{Vec, VecObject, VecObjectAlloc}, PersistentHashMap},
    object::{Object, ObjectBuilder},
};
use twizzler_rt_abi::object::MapFlags;

fn main() {
    let mut phm: PersistentHashMap<u32, u32> = PersistentHashMap::new(ObjectBuilder::default()).unwrap();
    println!("made hashmap with 0 entries!");

    phm.insert(1, 5).unwrap();
    println!("inserted one entry");
    println!("fetched 1: {:?}", phm.get(&1));

}