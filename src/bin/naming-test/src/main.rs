#![allow(dead_code)]

use std::env::current_dir;

use naming_core::{Entry, EntryType, NameSession, NameStore};

fn test_single_put_then_get() {
    println!("doing test_single_put_then_get");
    let store = NameStore::new();

    let session = store.root_session();
    assert_eq!(session.put("foo", EntryType::Object(1)), Ok(()));
    assert_eq!(session.get("foo"), Entry::try_new("foo", EntryType::Object(1)));
}

fn test_multi_put_then_get() {
    println!("doing test_multi_put_then_get");

    let store = NameStore::new();

    let session = store.root_session();

    for i in 0..10 {
        for j in 0..10 {
            assert_eq!(session.put(j.to_string(), EntryType::Object(i)), Ok(()));
        }

        for j in 0..10 {
            assert_eq!(session.get(j.to_string()), Entry::try_new(j.to_string(), EntryType::Object(i)));
        }
    }
}

fn put_namespace() {
    println!("doing put_namespace");

    let store = NameStore::new();

    let session = store.root_session();

    assert_eq!(session.put("namespace", EntryType::Namespace), Ok(()));
    assert_eq!(session.put("foo", EntryType::Object(1)), Ok(()));
    assert_eq!(session.put("namespace/foo", EntryType::Object(2)), Ok(()));

    assert_eq!(session.get("foo"), Entry::try_new("foo", EntryType::Object(1)));
    assert_eq!(session.get("namespace/foo"), Entry::try_new("foo", EntryType::Object(2)));
    assert_eq!(session.get("namespace"), Entry::try_new("namespace", EntryType::Namespace));
}

fn put_namespace_nested() {
    println!("doing put_namespace_nested");

    let store = NameStore::new();

    let session = store.root_session();

    assert_eq!(session.put("namespace", EntryType::Namespace), Ok(()));
    assert_eq!(session.put("namespace/namespace", EntryType::Namespace), Ok(()));
    assert_eq!(session.put("foo", EntryType::Object(1)), Ok(()));
    assert_eq!(session.put("namespace/foo", EntryType::Object(2)), Ok(()));
    assert_eq!(session.put("namespace/namespace/foo", EntryType::Object(3)), Ok(()));

    assert_eq!(session.get("foo"), Entry::try_new("foo", EntryType::Object(1)));
    assert_eq!(session.get("namespace/foo"), Entry::try_new("foo", EntryType::Object(2)));
    assert_eq!(session.get("namespace/namespace/foo"), Entry::try_new("foo", EntryType::Object(3)));
    assert_eq!(session.get("namespace"), Entry::try_new("namespace", EntryType::Namespace));
    assert_eq!(session.get("namespace/namespace"), Entry::try_new("namespace", EntryType::Namespace));
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

fn main() {
    //test_single_put_then_get();
    //test_multi_put_then_get();
    //put_namespace();
    put_namespace_nested();
}
