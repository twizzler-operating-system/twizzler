use twizzler_rt_abi::{
    error::{NamingError, TwzError},
    object::ObjID,
};

use super::*;

fn get_ok(session: &NameSession, name: &str) -> NsNode {
    session.get(name, GetFlags::empty()).unwrap()
}

#[test]
fn single_put_then_get() {
    let store = NameStore::new();
    let session = store.root_session();
    session.put("foo", ObjID::new(1)).unwrap();

    let node = get_ok(&session, "foo");
    assert_eq!(node.id, ObjID::new(1));
    assert_eq!(node.kind, NsNodeKind::Object);
}

#[test]
fn multi_put_then_get() {
    let store = NameStore::new();
    let session = store.root_session();
    for i in 0..100u128 {
        session.put(format!("k{i}"), ObjID::new(i)).unwrap();
    }
    for i in 0..100u128 {
        assert_eq!(get_ok(&session, &format!("k{i}")).id, ObjID::new(i));
    }
}

#[test]
fn put_namespace() {
    let store = NameStore::new();
    let session = store.root_session();
    session.mkns("namespace", false).unwrap();
    session.put("foo", ObjID::new(1)).unwrap();
    session.put("namespace/foo", ObjID::new(2)).unwrap();

    assert_eq!(get_ok(&session, "foo").id, ObjID::new(1));
    assert_eq!(get_ok(&session, "namespace/foo").id, ObjID::new(2));
    assert_eq!(get_ok(&session, "namespace").kind, NsNodeKind::Namespace);
}

#[test]
fn namespace_nested() {
    let store = NameStore::new();
    let session = store.root_session();
    session.mkns("namespace", false).unwrap();
    session.mkns("namespace/namespace", false).unwrap();
    session.put("foo", ObjID::new(1)).unwrap();
    session.put("namespace/foo", ObjID::new(2)).unwrap();
    session
        .put("namespace/namespace/foo", ObjID::new(3))
        .unwrap();

    assert_eq!(get_ok(&session, "foo").id, ObjID::new(1));
    assert_eq!(get_ok(&session, "namespace/foo").id, ObjID::new(2));
    assert_eq!(
        get_ok(&session, "namespace/namespace/foo").id,
        ObjID::new(3)
    );
    assert_eq!(get_ok(&session, "namespace").kind, NsNodeKind::Namespace);
    assert_eq!(
        get_ok(&session, "namespace/namespace").kind,
        NsNodeKind::Namespace
    );
}

#[test]
fn traverse_relative_vs_absolute() {
    let store = NameStore::new();
    let mut session = store.root_session();
    session.mkns("namespace", false).unwrap();
    session.put("foo", ObjID::new(0)).unwrap();

    session.change_namespace("namespace").unwrap();
    session.put("foo", ObjID::new(1)).unwrap();

    // Relative lookup sees the shadowing entry in the current namespace...
    assert_eq!(get_ok(&session, "foo").id, ObjID::new(1));
    // ...while an absolute path always starts back at root.
    assert_eq!(get_ok(&session, "/foo").id, ObjID::new(0));
}

#[test]
fn traverse_dot_and_dotdot() {
    let store = NameStore::new();
    let mut session = store.root_session();
    session.mkns("namespace", false).unwrap();
    session.put("foo", ObjID::new(0)).unwrap();
    session.put("baz", ObjID::new(0)).unwrap();

    assert_eq!(
        session.change_namespace("foo").unwrap_err(),
        TwzError::Naming(NamingError::WrongNameKind)
    );
    assert_eq!(
        session.change_namespace("bux").unwrap_err(),
        TwzError::Naming(NamingError::NotFound)
    );
    session.change_namespace("namespace").unwrap();
    session.put("foo", ObjID::new(1)).unwrap();
    session.put("baz", ObjID::new(1)).unwrap();

    // No nested "namespace" entry inside "namespace" itself.
    assert_eq!(
        session.change_namespace("namespace").unwrap_err(),
        TwzError::Naming(NamingError::NotFound)
    );

    assert_eq!(get_ok(&session, "foo").id, ObjID::new(1));
    assert_eq!(get_ok(&session, "../foo").id, ObjID::new(0));

    session.change_namespace(".").unwrap();
    assert_eq!(get_ok(&session, "foo").id, ObjID::new(1));
    assert_eq!(get_ok(&session, "/foo").id, ObjID::new(0));

    session.change_namespace("..").unwrap();
    assert_eq!(get_ok(&session, "foo").id, ObjID::new(0));
    assert_eq!(get_ok(&session, "namespace/foo").id, ObjID::new(1));
    assert_eq!(get_ok(&session, "namespace/../foo").id, ObjID::new(0));
}

#[test]
fn remove_then_readd() {
    let store = NameStore::new();
    let session = store.root_session();
    session.put("a", ObjID::new(1)).unwrap();
    assert_eq!(get_ok(&session, "a").id, ObjID::new(1));

    session.remove("a").unwrap();
    assert_eq!(
        session.get("a", GetFlags::empty()).unwrap_err(),
        TwzError::Naming(NamingError::NotFound)
    );

    session.put("a", ObjID::new(2)).unwrap();
    assert_eq!(get_ok(&session, "a").id, ObjID::new(2));
}

#[test]
fn remove_nested_bottom_up() {
    // `remove` only detaches the named entry from its immediate parent: there is no
    // "refuse to remove a non-empty namespace" check anywhere in the store today, and no
    // recursive-delete option (unlike the old, pre-rewrite naming-test crate this replaces).
    // Tearing down a subtree means removing its contents first, bottom-up, which is what this
    // test exercises alongside confirming siblings are left alone.
    let store = NameStore::new();
    let session = store.root_session();
    session.mkns("a", false).unwrap();
    session.mkns("b", false).unwrap();
    session.mkns("a/c", false).unwrap();
    session.put("a/i", ObjID::new(1)).unwrap();
    session.put("b/j", ObjID::new(2)).unwrap();
    session.put("g", ObjID::new(3)).unwrap();

    session.remove("a/i").unwrap();
    session.remove("a/c").unwrap();
    session.remove("a").unwrap();

    assert_eq!(
        session.get("a", GetFlags::empty()).unwrap_err(),
        TwzError::Naming(NamingError::NotFound)
    );
    assert_eq!(get_ok(&session, "b/j").id, ObjID::new(2));
    assert_eq!(get_ok(&session, "g").id, ObjID::new(3));
}

#[test]
fn reload_from_object_id() {
    let id = {
        let store = NameStore::new();
        let session = store.root_session();
        session.put("a", ObjID::new(42)).unwrap();
        store.id()
    };

    let store = NameStore::new_with_root(id).expect("store should reload from its own root id");
    let session = store.root_session();
    assert_eq!(get_ok(&session, "a").id, ObjID::new(42));
}