use std::fs::File;
use std::env::current_dir;

use naming_core::{Entry, EntryType, NameSession, NameStore};

fn test_single_put_then_get() {
    println!("doing test_single_put_then_get");
    let store = NameStore::new();

    let session = store.root_session();
    assert!(session.put("foo", EntryType::Object(1)) == Ok(()));
    assert!(session.get("foo") == Entry::try_new("foo", EntryType::Object(1)));
}

fn test_multi_put_then_get() {
    println!("doing test_multi_put_then_get");

    let store = NameStore::new();

    let session = store.root_session();

    for i in 0..10 {
        for j in 0..10 {
            assert!(session.put(j.to_string(), EntryType::Object(i)) == Ok(()));
        }

        for j in 0..10 {
            assert!(session.get(j.to_string()) == Entry::try_new(j.to_string(), EntryType::Object(i)));
        }
    }
}

fn put_namespace() {
    println!("doing put_namespace");

    let store = NameStore::new();

    let session = store.root_session();

    assert!(session.put("namespace", EntryType::Namespace) == Ok(()));
    assert!(session.put("foo", EntryType::Object(1)) == Ok(()));
    assert!(session.put("namespace/foo", EntryType::Object(2)) == Ok(()));

    assert!(session.get("foo") == Entry::try_new("foo", EntryType::Object(1)));
    assert!(session.get("namespace/foo") == Entry::try_new("foo", EntryType::Object(2)));
    assert!(session.get("namespace") == Entry::try_new("namespace", EntryType::Namespace));
}

fn put_namespace_nested() {
    println!("doing put_namespace_nested");

    let store = NameStore::new();

    let session = store.root_session();

    assert!(session.put("namespace", EntryType::Namespace) == Ok(()));
    assert!(session.put("namespace/namespace", EntryType::Namespace) == Ok(()));
    assert!(session.put("foo", EntryType::Object(1)) == Ok(()));
    assert!(session.put("namespace/foo", EntryType::Object(2)) == Ok(()));
    assert!(session.put("namespace/namespace/foo", EntryType::Object(3)) == Ok(()));

    assert!(session.get("foo") == Entry::try_new("foo", EntryType::Object(1)));
    assert!(session.get("namespace/foo") == Entry::try_new("foo", EntryType::Object(2)));
    assert!(session.get("namespace/namespace/foo") == Entry::try_new("foo", EntryType::Object(3)));
    assert!(session.get("namespace") == Entry::try_new("namespace", EntryType::Namespace));
    assert!(session.get("namespace/namespace") == Entry::try_new("namespace", EntryType::Namespace));
}

fn test_traverse_namespace() {

}

fn test_add_namespace() {

}

fn test_deep_namespace() {

}

fn test_root_skip() {

}

fn test_parent_parent() {

}

fn test_recursive_parent_traverse() {

}

fn test_parent_root() {

}

fn test_pre_order_traversal() {

}

fn test_with_concurrent_sessions() {

}

use twizzler::collections::vec::{VecObject, VecObjectAlloc};
use twizzler::object::ObjectBuilder;
use twizzler::marker::Invariant;

struct Foo([u8; 300], i32, u128);

unsafe impl Invariant for Foo {}
 
fn main() {
    /*let mut store = VecObject::<Foo, VecObjectAlloc>::new(ObjectBuilder::default()).unwrap();
    println!("{:?}", store.push(Foo([0u8; 300], 0, 0)));
    println!("{:?}", store.len());
    println!("{:?}", store.len());
    println!("{:?}", store.get(0));
    println!("{:?}", store.get(0));
    println!("{:?}", store.len());
    println!("{:?}", store.push(Foo([0u8; 300], 0, 0)));
    println!("{:?}", store.len());
    println!("{:?}", store.len());
    println!("{:?}", store.get(0));
    println!("{:?}", store.get(1));
    println!("{:?}", store.get(0));
    println!("{:?}", store.len());
    println!("{:?}", store.push(Foo([0u8; 300], 0, 0)));
    println!("{:?}", store.len());
    println!("{:?}", store.len());
    println!("{:?}",  store.get(0));*/
    
    
    put_namespace();
}
