//! Directory removal over both namespace kinds: native (namespace objects) and external
//! (ext4, under `/ext`). `remove_dir_all` is the operation cargo's build scripts fail on --
//! proc-macro2 removes its probe directory and `process::exit(1)`s if it cannot.

use std::{
    fs,
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
    process::exit,
};

struct Results {
    pass: u32,
    fail: u32,
}

impl Results {
    fn check(&mut self, name: &str, ok: bool, detail: impl std::fmt::Display) {
        if ok {
            self.pass += 1;
            println!("RMDIRTEST PASS {}: {}", name, detail);
        } else {
            self.fail += 1;
            println!("RMDIRTEST FAIL {}: {}", name, detail);
        }
    }
}

/// A directory with two files and a populated subdirectory: enough that a non-recursive
/// removal must refuse it, and that a recursive one has to descend.
fn populate(dir: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    for name in ["a", "b"] {
        fs::File::create(dir.join(name))?.write_all(b"hello")?;
    }
    fs::create_dir(dir.join("sub"))?;
    fs::File::create(dir.join("sub").join("c"))?.write_all(b"nested")?;
    Ok(())
}

fn run(kind: &str, root: &Path, r: &mut Results) {
    let _ = fs::remove_dir_all(root);
    if let Err(e) = fs::create_dir_all(root) {
        r.check(
            &format!("{}/setup", kind),
            false,
            format!("create {:?}: {}", root, e),
        );
        return;
    }

    // 1. The reported failure: a populated tree removed in one call.
    let tree = root.join("tree");
    match populate(&tree) {
        Ok(()) => match fs::remove_dir_all(&tree) {
            Ok(()) => {
                let gone = !tree.exists();
                r.check(
                    &format!("{}/remove_dir_all", kind),
                    gone,
                    if gone {
                        "removed".into()
                    } else {
                        format!("{:?} still exists", tree)
                    },
                );
            }
            Err(e) => r.check(&format!("{}/remove_dir_all", kind), false, e),
        },
        Err(e) => r.check(
            &format!("{}/remove_dir_all", kind),
            false,
            format!("populate: {}", e),
        ),
    }

    // 2. rmdir must refuse a non-empty directory rather than report a success it did not perform --
    //    `remove_dir_all` above ignores NotFound on its final rmdir, so a misreported error would
    //    read as a removal that silently did nothing.
    let busy = root.join("busy");
    match populate(&busy) {
        Ok(()) => match fs::remove_dir(&busy) {
            Ok(()) => r.check(
                &format!("{}/rmdir_nonempty", kind),
                false,
                format!(
                    "removed a non-empty directory; still there: {}",
                    busy.exists()
                ),
            ),
            Err(e) => {
                let kind_ok = e.kind() == ErrorKind::DirectoryNotEmpty;
                r.check(
                    &format!("{}/rmdir_nonempty", kind),
                    kind_ok,
                    format!("{:?} ({})", e.kind(), e),
                );
                let _ = fs::remove_dir_all(&busy);
            }
        },
        Err(e) => r.check(
            &format!("{}/rmdir_nonempty", kind),
            false,
            format!("populate: {}", e),
        ),
    }

    // 3. rmdir of an empty directory.
    let empty = root.join("empty");
    match fs::create_dir(&empty).and_then(|()| fs::remove_dir(&empty)) {
        Ok(()) => {
            let gone = !empty.exists();
            r.check(
                &format!("{}/rmdir_empty", kind),
                gone,
                if gone { "removed" } else { "still there" },
            )
        }
        Err(e) => r.check(&format!("{}/rmdir_empty", kind), false, e),
    }

    // 4. proc-macro2's exact shape: OUT_DIR/probe holding what rustc emitted, removed whole.
    let probe = root.join("build/proc-macro2-0000/out/probe");
    let res = fs::create_dir_all(&probe).and_then(|()| {
        fs::File::create(probe.join("libproc_macro2.rmeta"))?.write_all(b"x")?;
        fs::File::create(probe.join("probe.d"))?.write_all(b"y")?;
        fs::remove_dir_all(&probe)
    });
    match res {
        Ok(()) => {
            let gone = !probe.exists();
            r.check(
                &format!("{}/probe_cleanup", kind),
                gone,
                if gone { "removed" } else { "still there" },
            )
        }
        Err(e) => r.check(&format!("{}/probe_cleanup", kind), false, e),
    }

    let _ = fs::remove_dir_all(root);
}

/// Where external unlink actually breaks: at the root, inside a directory the host disk
/// builder made, or only inside one the guest made. `path_of` walks `..` up to the root and
/// fails with InvalidInput if a link is missing, so these three separate its failure modes.
fn unlink_probes(r: &mut Results) {
    let cases: [(&str, PathBuf); 3] = [
        ("ext_root_file", PathBuf::from("/ext/rmdirtest_probe_a")),
        (
            "ext_builder_dir_file",
            PathBuf::from("/ext/sysroot/rmdirtest_probe_b"),
        ),
        (
            "ext_guest_dir_file",
            PathBuf::from("/ext/rmdirtest_probe_dir/c"),
        ),
    ];
    for (name, path) in cases {
        if let Some(parent) = path.parent() {
            if let Err(e) = fs::create_dir_all(parent) {
                r.check(name, false, format!("mkdir {:?}: {}", parent, e));
                continue;
            }
        }
        match fs::File::create(&path).and_then(|mut f| f.write_all(b"x")) {
            Ok(()) => match fs::remove_file(&path) {
                Ok(()) => r.check(name, !path.exists(), "unlinked"),
                Err(e) => r.check(name, false, format!("{:?} ({})", e.kind(), e)),
            },
            Err(e) => r.check(name, false, format!("create {:?}: {}", path, e)),
        }
    }
    let _ = fs::remove_dir_all("/ext/rmdirtest_probe_dir");
}

fn main() {
    let mut r = Results { pass: 0, fail: 0 };
    unlink_probes(&mut r);
    run("native", &PathBuf::from("/rmdirtest"), &mut r);
    run("external", &PathBuf::from("/ext/rmdirtest"), &mut r);
    println!("RMDIRTEST DONE: {} passed, {} failed", r.pass, r.fail);
    exit(if r.fail == 0 { 0 } else { 1 });
}
