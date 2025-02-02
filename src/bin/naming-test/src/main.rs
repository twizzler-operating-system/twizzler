#![allow(dead_code)]

use naming_core::{Entry, EntryType, ErrorKind, NameStore};

fn test_single_put_then_get() {
    println!("doing test_single_put_then_get");
    let store = NameStore::new();
    let session = store.root_session();
    assert_eq!(session.put("foo", EntryType::Object(1)), Ok(()));
    assert_eq!(
        session.get("foo"),
        Entry::try_new("foo", EntryType::Object(1))
    );
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
            assert_eq!(
                session.get(j.to_string()),
                Entry::try_new(j.to_string(), EntryType::Object(i))
            );
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

    assert_eq!(
        session.get("foo"),
        Entry::try_new("foo", EntryType::Object(1))
    );
    assert_eq!(
        session.get("namespace/foo"),
        Entry::try_new("foo", EntryType::Object(2))
    );
    assert_eq!(
        session.get("namespace"),
        Entry::try_new("namespace", EntryType::Namespace)
    );
}

fn namespace_nested() {
    println!("doing put_namespace_nested");

    let store = NameStore::new();

    let session = store.root_session();

    assert_eq!(session.put("namespace", EntryType::Namespace), Ok(()));
    assert_eq!(
        session.put("namespace/namespace", EntryType::Namespace),
        Ok(())
    );
    assert_eq!(session.put("foo", EntryType::Object(1)), Ok(()));
    assert_eq!(session.put("namespace/foo", EntryType::Object(2)), Ok(()));
    assert_eq!(
        session.put("namespace/namespace/foo", EntryType::Object(3)),
        Ok(())
    );

    assert_eq!(
        session.get("foo"),
        Entry::try_new("foo", EntryType::Object(1))
    );
    assert_eq!(
        session.get("namespace/foo"),
        Entry::try_new("foo", EntryType::Object(2))
    );
    assert_eq!(
        session.get("namespace/namespace/foo"),
        Entry::try_new("foo", EntryType::Object(3))
    );
    assert_eq!(
        session.get("namespace"),
        Entry::try_new("namespace", EntryType::Namespace)
    );
    assert_eq!(
        session.get("namespace/namespace"),
        Entry::try_new("namespace", EntryType::Namespace)
    );
}

fn traverse_namespace_nested_1() {
    println!("doing test_traverse_namespace");

    let store = NameStore::new();

    let mut session = store.root_session();

    assert_eq!(session.put("namespace", EntryType::Namespace), Ok(()));
    assert_eq!(session.put("foo", EntryType::Object(0)), Ok(()));

    assert_eq!(session.change_namespace("namespace"), Ok(()));
    assert_eq!(session.put("foo", EntryType::Object(1)), Ok(()));

    assert_eq!(
        session.get("foo"),
        Entry::try_new("foo", EntryType::Object(1))
    );
    assert_eq!(
        session.get("/foo"),
        Entry::try_new("foo", EntryType::Object(0))
    );
}

fn traverse_namespace_nested_2() {
    println!("doing test_traverse_namespace_2");

    let store = NameStore::new();

    let mut session = store.root_session();

    assert_eq!(session.put("namespace", EntryType::Namespace), Ok(()));
    assert_eq!(session.put("foo", EntryType::Object(0)), Ok(()));
    assert_eq!(session.put("baz", EntryType::Object(0)), Ok(()));

    assert_eq!(
        session.change_namespace("foo"),
        Err(ErrorKind::NotNamespace)
    );
    assert_eq!(session.change_namespace("bux"), Err(ErrorKind::NotFound));
    assert_eq!(session.change_namespace("namespace"), Ok(()));

    assert_eq!(session.put("foo", EntryType::Object(1)), Ok(()));
    assert_eq!(session.put("baz", EntryType::Object(1)), Ok(()));

    assert_eq!(
        session.change_namespace("namespace"),
        Err(ErrorKind::NotFound)
    );

    assert_eq!(
        session.get("foo"),
        Entry::try_new("foo", EntryType::Object(1))
    );
    assert_eq!(
        session.get("baz"),
        Entry::try_new("baz", EntryType::Object(1))
    );
    assert_eq!(
        session.get("../foo"),
        Entry::try_new("foo", EntryType::Object(0))
    );
    assert_eq!(
        session.get("../baz"),
        Entry::try_new("baz", EntryType::Object(0))
    );

    assert_eq!(session.change_namespace("."), Ok(()));

    assert_eq!(
        session.get("foo"),
        Entry::try_new("foo", EntryType::Object(1))
    );
    assert_eq!(
        session.get("baz"),
        Entry::try_new("baz", EntryType::Object(1))
    );
    assert_eq!(
        session.get("../foo"),
        Entry::try_new("foo", EntryType::Object(0))
    );
    assert_eq!(
        session.get("../baz"),
        Entry::try_new("baz", EntryType::Object(0))
    );
    assert_eq!(
        session.get("/foo"),
        Entry::try_new("foo", EntryType::Object(0))
    );
    assert_eq!(
        session.get("/baz"),
        Entry::try_new("baz", EntryType::Object(0))
    );

    assert_eq!(session.change_namespace(".."), Ok(()));

    assert_eq!(
        session.get("foo"),
        Entry::try_new("foo", EntryType::Object(0))
    );
    assert_eq!(
        session.get("baz"),
        Entry::try_new("baz", EntryType::Object(0))
    );
    assert_eq!(
        session.get("namespace/foo"),
        Entry::try_new("foo", EntryType::Object(1))
    );
    assert_eq!(
        session.get("namespace/baz"),
        Entry::try_new("baz", EntryType::Object(1))
    );
    assert_eq!(
        session.get("namespace/../foo"),
        Entry::try_new("foo", EntryType::Object(0))
    );
    assert_eq!(
        session.get("namespace/../baz"),
        Entry::try_new("baz", EntryType::Object(0))
    );
    assert_eq!(
        session.get("/../namespace/../foo"),
        Entry::try_new("foo", EntryType::Object(0))
    );
    assert_eq!(
        session.get("/../namespace/../baz"),
        Entry::try_new("baz", EntryType::Object(0))
    );
    assert_eq!(
        session.get("/../namespace/../namespace/foo"),
        Entry::try_new("foo", EntryType::Object(1))
    );
    assert_eq!(
        session.get("/../namespace/../namespace/baz"),
        Entry::try_new("baz", EntryType::Object(1))
    );
    assert_eq!(
        session.get("/.././.././foo"),
        Entry::try_new("foo", EntryType::Object(0))
    );
    assert_eq!(
        session.get("/.././.././baz"),
        Entry::try_new("baz", EntryType::Object(0))
    );
    assert_eq!(
        session.get("/../namespace/namespace/../../foo"),
        Err(ErrorKind::NotFound)
    );
    assert_eq!(
        session.get("/../namespace/namespace/../../baz"),
        Err(ErrorKind::NotFound)
    );
}

fn remove() {
    println!("doing remove");

    let store = NameStore::new();

    let mut session = store.root_session();
    session.put("/a", EntryType::Object(1));
    assert_eq!(session.get("/a"), Entry::try_new("a", EntryType::Object(1)));
    assert_eq!(session.remove("a", false), Ok(()));
    assert_eq!(session.get("/a"), Err(ErrorKind::NotFound));

    session.put("/a", EntryType::Object(1));
    assert_eq!(session.get("/a"), Entry::try_new("a", EntryType::Object(1)));
}

fn remove_nested() {
    println!("doing remove_nested");

    let store = NameStore::new();

    let mut session = store.root_session();
    session.put("/b", EntryType::Object(1));
    session.put("/c", EntryType::Object(1));
    session.put("/a", EntryType::Namespace);
    session.put("/a/a", EntryType::Namespace);
    session.put("/a/a/a", EntryType::Object(1));
    session.put("/a/a/b", EntryType::Object(2));
    
    assert_eq!(session.get("/a/a/a"), Entry::try_new("a", EntryType::Object(1)));
    assert_eq!(session.remove("/a/a", false), Err(ErrorKind::NotFile));
    assert_eq!(session.remove("/a/a/a", false), Ok(()));

    assert_eq!(session.remove("b", false), Ok(()));
    assert_eq!(session.remove("c", false), Ok(()));
    assert_eq!(session.get("/a/a/a"), Err(ErrorKind::NotFound));
    assert_eq!(session.get("/a/a/b"), Entry::try_new("b", EntryType::Object(2)));
}

fn remove_recursive() {
    println!("doing remove_recursive");

    let store = NameStore::new();

    let mut session = store.root_session();

    session.put("/a", EntryType::Namespace);
    session.put("/b", EntryType::Namespace);
    session.put("/a/c", EntryType::Namespace);
    session.put("/a/d", EntryType::Namespace);
    session.put("/b/e", EntryType::Namespace);
    session.put("/b/f", EntryType::Namespace);
    session.put("/g", EntryType::Object(0));
    session.put("/h", EntryType::Object(1));
    session.put("/a/i", EntryType::Object(0));
    session.put("/b/j", EntryType::Object(1));
    
    assert_eq!(session.remove("a", true), Ok(()));
    assert_eq!(session.remove("a", true), Err(ErrorKind::NotFound));

    assert_eq!(session.get("b"), Entry::try_new("b", EntryType::Namespace));
    assert_eq!(session.get("b/e"), Entry::try_new("e", EntryType::Namespace));
    assert_eq!(session.get("b/f"), Entry::try_new("f", EntryType::Namespace));
    assert_eq!(session.get("g"), Entry::try_new("g", EntryType::Object(0)));
    assert_eq!(session.get("h"), Entry::try_new("h", EntryType::Object(1)));
    
    assert_eq!(session.remove("b", true), Ok(()));

    assert_eq!(session.get("e"), Err(ErrorKind::NotFound));
    assert_eq!(session.get("f"), Err(ErrorKind::NotFound));
    assert_eq!(session.get("g"), Entry::try_new("g", EntryType::Object(0)));
    assert_eq!(session.get("h"), Entry::try_new("h", EntryType::Object(1)));
}

fn main() {
    test_single_put_then_get();
    test_multi_put_then_get();
    put_namespace();
    namespace_nested();
    traverse_namespace_nested_1();
    traverse_namespace_nested_2();
    remove();
    remove_nested();
    remove_recursive();
}
