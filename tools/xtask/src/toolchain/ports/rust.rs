use std::{
    fs::File,
    io::{Read, Write},
    path::Path,
    process::Command,
};

use guess_host_triple::guess_host_triple;
use toml_edit::{Array, DocumentMut};

use crate::{toolchain::bootstrap::setup_logfile, triple::Triple};

pub fn install(triple: &Triple) -> anyhow::Result<()> {
    println!("Building rust for {}", triple);
    generate_native_config_toml(triple)?;
    build_rust(triple)
}

fn build_rust(triple: &Triple) -> anyhow::Result<()> {
    std::env::set_var("BOOTSTRAP_SKIP_TARGET_SANITY", "1");
    // twizzler-rt-abi's build.rs needs these to find the mlibc headers and a matching
    // libclang when bindgen runs for the twizzler target under x.py.
    std::env::set_var(
        "TWIZZLER_ABI_SYSROOTS",
        Path::new("toolchain/install/sysroots").canonicalize()?,
    );
    std::env::set_var(
        "TWIZZLER_ABI_LLVM_CONFIG",
        Path::new("toolchain/install/bin/llvm-config").canonicalize()?,
    );
    // Bootstrap appends these to the CFLAGS/CXXFLAGS it hands to build scripts (cc-rs), which
    // otherwise invoke clang for the twizzler target with no sysroot (e.g. rustc_llvm's
    // llvm-wrapper fails on missing libc/libc++ headers).
    let sysroot = Path::new("toolchain/install/sysroots")
        .join(triple.to_string())
        .canonicalize()?;
    let triple_underscored = triple.to_string().replace('-', "_");
    std::env::set_var(
        format!("CFLAGS_{}", triple_underscored),
        format!("--sysroot={}", sysroot.display()),
    );
    std::env::set_var(
        format!("CXXFLAGS_{}", triple_underscored),
        format!("--sysroot={}", sysroot.display()),
    );

    // cargo's -sys crates (openssl-sys, curl-sys, libgit2-sys, libssh2-sys, libz-sys) have to
    // find the ports in the sysroot rather than vendoring and cross-building their own C.
    // libcurl.pc lists `Requires.private: libnghttp2`, so nghttp2 has to be resolvable here too
    // or pkg-config fails the whole libcurl query.
    // pkg-config refuses to answer at all under cross-compilation without ALLOW_CROSS, and
    // LIBDIR (not PATH) *replaces* the host's search path -- with PATH, a host libcurl wins and
    // links a host library into a twizzler binary. SYSROOT_DIR re-roots the ports' on-target
    // `/pkg/<name>` prefixes onto the host, which is the same mechanism ports/libgit2.rs
    // already uses to consume its own dependencies.
    let pkgconfig = ["openssl", "curl", "libgit2", "libssh2", "nghttp2", "zlib"]
        .iter()
        .map(|p| format!("{}/pkg/{}/lib/pkgconfig", sysroot.display(), p))
        .collect::<Vec<_>>()
        .join(":");
    std::env::set_var(format!("PKG_CONFIG_ALLOW_CROSS_{}", triple), "1");
    std::env::set_var(format!("PKG_CONFIG_LIBDIR_{}", triple), pkgconfig);
    std::env::set_var(
        format!("PKG_CONFIG_SYSROOT_DIR_{}", triple),
        sysroot.display().to_string(),
    );
    std::env::set_var("LIBGIT2_SYS_USE_PKG_CONFIG", "1");
    std::env::set_var("LIBSSH2_SYS_USE_PKG_CONFIG", "1");
    std::env::set_var("OPENSSL_NO_VENDOR", "1");

    let log = setup_logfile("ports/rust", "xtask-install", Some(triple))?;
    // --host: `build.host` also lists the build machine, so without this the Cargo tool step
    // (IS_HOST) builds cargo for the *host* as well -- pulling libgit2-sys/libssh2-sys into a
    // build that resolves against the twizzler ports and dies on "skipping incompatible
    // libgit2.so". A host-native cargo installed into the twizzler sysroot prefix was never
    // wanted anyway; this port exists to produce the twizzler-hosted toolchain.
    let status = Command::new("./x.py")
        .arg("install")
        .arg("--host")
        .arg(triple.to_string())
        .stdout(log.try_clone()?)
        .stderr(log)
        .current_dir("toolchain/src/rust")
        .status()?;
    if !status.success() {
        anyhow::bail!(
            "failed to compile rust toolchain (see toolchain/install/build/ports/rust/{}/xtask-install.log)",
            triple
        );
    }

    let src_log = setup_logfile("ports/rust", "xtask-install-src", Some(triple))?;
    let src_status = Command::new("./x.py")
        .arg("install")
        .arg("src")
        .stdout(src_log.try_clone()?)
        .stderr(src_log)
        .current_dir("toolchain/src/rust")
        .status()?;
    if !src_status.success() {
        anyhow::bail!(
            "failed to install rust source (see toolchain/install/build/ports/rust/{}/xtask-install-src.log)",
            triple
        );
    }

    Ok(())
}

fn generate_native_config_toml(triple: &Triple) -> anyhow::Result<()> {
    /* We need to add two(ish) things to the config.toml for rustc: the paths of tools for each twizzler target (built by LLVM as part
    of rustc), and the host triple (added to the list of triples to support). */
    let mut data = File::open("toolchain/src/config.toml")?;
    let mut buf = String::new();
    data.read_to_string(&mut buf)?;
    let commented =
        String::from("# This file was auto-generated by xtask. Do not edit directly.\n") + &buf;
    let mut toml = commented.parse::<DocumentMut>()?;
    let llvm_bin = Path::new("toolchain/install/bin").canonicalize()?;
    let tstr = &triple.to_string();
    // x.py runs with cwd toolchain/src/rust, so the prefix must be absolute.
    let install_prefix = Path::new("toolchain/install/sysroots")
        .join(tstr)
        .join("pkg/rust");
    // Remove any previous install: x.py overlays hashed artifacts without pruning, leaving
    // stale duplicates (e.g. two libstd-<hash>.so) that break crate resolution on-target.
    let _ = std::fs::remove_dir_all(&install_prefix);
    std::fs::create_dir_all(&install_prefix)?;
    let install_prefix = install_prefix.canonicalize()?;
    let build_dir = Path::new("toolchain/install/build/ports/rust").join(tstr);
    let sysroot_dir = Path::new("toolchain/install/sysroots")
        .join(tstr)
        .canonicalize()?;

    std::fs::create_dir_all(&build_dir)?;
    let build_dir = build_dir.canonicalize()?;

    toml["build"]["target"].as_array_mut().unwrap().push(tstr);
    toml["build"]["host"]
        .as_array_mut()
        .unwrap()
        .push(guess_host_triple().unwrap());
    toml["build"]["host"].as_array_mut().unwrap().push(tstr);
    toml["install"]["prefix"] = toml_edit::value(install_prefix.display().to_string());

    // bootstrap's `tool_enabled` (src/bootstrap/src/lib.rs) requires BOTH of these before the
    // Cargo step will run. Std/Rustc/LlvmTools install unconditionally, so naming only cargo in
    // `tools` adds a component rather than dropping any.
    toml["build"]["extended"] = toml_edit::value(true);
    let mut tools = Array::new();
    tools.push("cargo");
    toml["build"]["tools"] = toml_edit::value(tools);

    let cc = llvm_bin
        .join("clang")
        .canonicalize()?
        .to_str()
        .unwrap()
        .to_string();
    let cxx = llvm_bin
        .join("clang++")
        .canonicalize()?
        .to_str()
        .unwrap()
        .to_string();
    // The twizzler target spec expects to drive ld.lld directly (bare crt object names resolved
    // via -L); a cc driver rejects those bare names before the linker runs.
    let ld = llvm_bin
        .join("ld.lld")
        .canonicalize()?
        .to_str()
        .unwrap()
        .to_string();
    let ar = llvm_bin
        .join("llvm-ar")
        .canonicalize()?
        .to_str()
        .unwrap()
        .to_string();

    toml["target"][tstr]["llvm-has-rust-patches"] = toml_edit::value(true);
    toml["target"][tstr]["cc"] = toml_edit::value(cc);
    toml["target"][tstr]["cxx"] = toml_edit::value(cxx);
    toml["target"][tstr]["linker"] = toml_edit::value(ld);
    toml["target"][tstr]["ar"] = toml_edit::value(ar);

    // Rustc-level -L (not link-arg): rustc resolves the spec's bare crt object names through its
    // own search paths and forwards the dir to the linker for -lc/-lc++.
    let mut rustflags_array = Array::new();
    rustflags_array.push("-L");
    rustflags_array.push(format!("{}/lib", sysroot_dir.display()));
    rustflags_array.push("-C");
    rustflags_array.push("link-arg=-z");
    rustflags_array.push("-C");
    rustflags_array.push("link-arg=norelro");
    // twz_rt_* symbols are bound at load time by dynlink, not DT_NEEDED.
    rustflags_array.push("-C");
    rustflags_array.push("link-arg=--allow-shlib-undefined");
    // libc++abi.so needs _Unwind_* at load time but rustc's version script keeps the in-tree
    // unwinder's symbols local, so force a DT_NEEDED on the shared libunwind. Trailing args
    // land after rustc's own --as-needed, so --no-as-needed here makes the record stick.
    rustflags_array.push("-C");
    rustflags_array.push("link-arg=--no-as-needed");
    rustflags_array.push("-C");
    rustflags_array.push("link-arg=-lunwind");

    // Marks this as the native std: the one a Twizzler-hosted rustc links programs against, where
    // `-ltwz_rt` can be emitted from libstd itself so a bare `rustc prog.rs` links without extra
    // flags. The cross-compiler's std is built without it (it builds libtwz_rt.so in the first
    // place). --check-cfg keeps the unexpected_cfgs lint quiet for every other crate built here.
    rustflags_array.push("--cfg");
    rustflags_array.push("twizzler_hosted");
    rustflags_array.push("--check-cfg");
    rustflags_array.push("cfg(twizzler_hosted)");

    toml["target"][tstr]["rustflags"] = toml_edit::value(rustflags_array);
    toml["target"][tstr]["llvm-libunwind"] = toml_edit::value("in-tree");

    toml["build"]["build-dir"] = toml_edit::value(build_dir.display().to_string());

    let mut out = File::create("toolchain/src/rust/bootstrap.toml")?;
    out.write_all(toml.to_string().as_bytes())?;
    Ok(())
}
