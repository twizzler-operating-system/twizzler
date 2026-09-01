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
    // `remove` only detaches the named entry from its immediate parent -- there is no
    // recursive-delete option (unlike the old, pre-rewrite naming-test crate this replaces), and
    // a non-empty namespace is refused outright (`remove_refuses_a_non_empty_namespace`).
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
fn over_long_component_rejected() {
    let store = NameStore::new();
    let session = store.root_session();
    let long = "x".repeat(crate::MAX_KEY_SIZE + 1);

    assert_eq!(
        session.put(&long, ObjID::new(1)).unwrap_err(),
        TwzError::INVALID_ARGUMENT
    );
    assert_eq!(
        session.mkns(&long, false).unwrap_err(),
        TwzError::INVALID_ARGUMENT
    );
    // Name and target each fit, but not together.
    assert_eq!(
        session
            .link("y".repeat(crate::MAX_KEY_SIZE - 1), "z".repeat(2))
            .unwrap_err(),
        TwzError::INVALID_ARGUMENT
    );
}

#[test]
fn traverse_through_object_fails() {
    let store = NameStore::new();
    let session = store.root_session();
    session.put("foo", ObjID::new(1)).unwrap();
    session.put("bar", ObjID::new(2)).unwrap();

    // "foo" is an object, so it cannot be walked through -- without this the lookup used to fall
    // through to the current namespace and hand back "/bar".
    assert_eq!(
        session.get("foo/bar", GetFlags::empty()).unwrap_err(),
        TwzError::Naming(NamingError::WrongNameKind)
    );
}

#[test]
fn rename_to_self_is_noop() {
    let store = NameStore::new();
    let session = store.root_session();
    session.put("a", ObjID::new(1)).unwrap();

    session.rename("a", "a").unwrap();
    assert_eq!(get_ok(&session, "a").id, ObjID::new(1));

    session.rename("a", "./a").unwrap();
    assert_eq!(get_ok(&session, "a").id, ObjID::new(1));

    session.rename("a", "b").unwrap();
    assert_eq!(get_ok(&session, "b").id, ObjID::new(1));
    assert_eq!(
        session.get("a", GetFlags::empty()).unwrap_err(),
        TwzError::Naming(NamingError::NotFound)
    );
}

#[test]
fn remove_refuses_a_non_empty_namespace() {
    let store = NameStore::new();
    let session = store.root_session();
    session.mkns("a", false).unwrap();
    session.put("a/i", ObjID::new(1)).unwrap();

    assert_eq!(
        session.remove("a").unwrap_err(),
        TwzError::Naming(NamingError::NotEmpty)
    );
    // Refused outright, not half-done: nothing here reclaims a namespace object, so a removal
    // that detached the entry would have orphaned "a/i" with no way to reach it.
    assert_eq!(get_ok(&session, "a").kind, NsNodeKind::Namespace);
    assert_eq!(get_ok(&session, "a/i").id, ObjID::new(1));

    session.remove("a/i").unwrap();
    session.remove("a").unwrap();
}

#[test]
fn remove_rejects_dot_and_dotdot() {
    let store = NameStore::new();
    let mut session = store.root_session();
    session.mkns("foo", false).unwrap();
    session.put("foo/a", ObjID::new(1)).unwrap();

    // Each of these used to unlink something: "foo/.." resolved to the node for "foo" in root,
    // and "." to the namespace's own self-entry.
    for path in ["foo/..", ".", "foo/.", "..", "/"] {
        assert_eq!(
            session.remove(path).unwrap_err(),
            TwzError::INVALID_ARGUMENT,
            "remove({path:?}) should be rejected"
        );
    }

    assert_eq!(get_ok(&session, "foo").kind, NsNodeKind::Namespace);
    assert_eq!(get_ok(&session, "foo/a").id, ObjID::new(1));
    // The self-entry survived, so "." is still traversable.
    session.change_namespace("foo").unwrap();
    session.change_namespace(".").unwrap();
    assert_eq!(get_ok(&session, "a").id, ObjID::new(1));
}

#[test]
fn rename_over_existing_name() {
    let store = NameStore::new();
    let session = store.root_session();
    session.put("a", ObjID::new(1)).unwrap();
    session.put("b", ObjID::new(2)).unwrap();

    session.rename("a", "b").unwrap();

    assert_eq!(get_ok(&session, "b").id, ObjID::new(1));
    assert_eq!(
        session.get("a", GetFlags::empty()).unwrap_err(),
        TwzError::Naming(NamingError::NotFound)
    );
    // Replace evicts rather than appending: a duplicate would shadow permanently, since `find`
    // and `remove` both stop at the first match.
    let bs = session
        .enumerate_namespace(".", 0, usize::MAX)
        .unwrap()
        .into_iter()
        .filter(|n| n.name().unwrap() == "b")
        .count();
    assert_eq!(bs, 1);
}

#[test]
fn rename_onto_namespace_rejected() {
    let store = NameStore::new();
    let session = store.root_session();
    session.mkns("d", false).unwrap();
    session.put("d/x", ObjID::new(1)).unwrap();
    session.put("a", ObjID::new(2)).unwrap();

    // Evicting "d" would orphan "d/x" -- nothing here deletes a namespace's contents, and nothing
    // reclaims the namespace object either.
    assert_eq!(
        session.rename("a", "d").unwrap_err(),
        TwzError::Naming(NamingError::AlreadyExists)
    );
    assert_eq!(get_ok(&session, "d/x").id, ObjID::new(1));
    assert_eq!(get_ok(&session, "a").id, ObjID::new(2));
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

/// `cwd_path` must still name a working namespace that no walk ever recorded a parent for.
///
/// Every namespace a walk produces carries an in-memory parent, so the ordinary path is read
/// straight off that chain. A namespace opened by id carries none. That gap is not hypothetical:
/// `..` out of such a namespace used to *invent* a parent -- `ParentInfo::new(namespace, "..")`,
/// a chain asserting that a namespace's parent is its own child -- which `cwd_path` would have
/// walked, emitting ".." until its depth bound stopped it.
///
/// Nothing reachable from userspace produces this state today, so no boot exercises the recovery
/// and no green suite is evidence about it. That is precisely why it is worth a test: without
/// one, working code and dead code that merely compiles look identical from here.
#[test]
fn cwd_path_recovers_a_parent_it_was_never_given() {
    let store = NameStore::new();
    let mut session = store.root_session();
    session.mkns("a", false).unwrap();
    session.mkns("a/b", false).unwrap();

    // Control: reached by a walk, so the chain exists and the path comes off it.
    session.change_namespace("/a/b").unwrap();
    assert_eq!(
        session.cwd_path().unwrap(),
        std::path::PathBuf::from("/a/b")
    );
    let b = session.cwd().id();

    // The state a walk cannot produce: the same namespace, opened by id, with no parent. Asserted
    // rather than assumed -- if this ever starts carrying a parent, the test below would pass for
    // the ordinary reason and stop testing recovery at all.
    let bare = session.open_namespace(b, false, None).unwrap();
    assert!(bare.parent().is_none());
    session.working_ns = Some(bare);

    // Recovered: the persisted ".." gives the parent, and the name is found where names actually
    // live -- as a binding in that parent.
    assert_eq!(
        session.cwd_path().unwrap(),
        std::path::PathBuf::from("/a/b")
    );
}
