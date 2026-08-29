use std::io::stderr;

#[allow(unreachable_code, unused)]
fn main() {
    let outdir = std::env::var("OUT_DIR").unwrap();
    let target = std::env::var("TARGET").unwrap();
    let cflags = std::env::var("CFLAGS").unwrap_or("".to_owned());
    let arch = target.split("-").next().unwrap();
    let cmake_build = format!("{}/cmake-build", outdir);

    // A/B ARM: lwext4's block cache is CONFIG_BLOCK_DEV_CACHE_SIZE blocks, default 1024 (4 MiB at
    // a 4 KiB block). `ext4_block_cache_flush` only walks the dirty list and never evicts clean
    // buffers, so metadata should stay cached -- and measured on 08-26 it did, with block reads
    // plateauing at 57,545 and stopping. From 08-27 they never plateau, which is the signature of
    // a working set crossing a fixed bound. Set here rather than via the CFLAGS environment so the
    // arm is visible in `git diff`; an env-var arm is invisible to every mtime and fingerprint
    // check we have.
    let cflags = format!(
        "{} -DCONFIG_USE_DEFAULT_CFG -DCONFIG_BLOCK_DEV_CACHE_SIZE=1024 -g",
        cflags
    );

    //let _ = std::fs::remove_dir_all(&cmake_build);

    let mut proc = std::process::Command::new("cmake");
    proc.current_dir("lwext4")
        .stdout(stderr())
        .arg("-DCMAKE_BUILD_TYPE=Release")
        .arg("-DCMAKE_POLICY_VERSION_MINIMUM=3.5")
        .arg("-DCMAKE_SYSTEM_NAME=Generic")
        .arg("-DLIB_ONLY=True")
        .arg("-DCONFIG_HAVE_OWN_ERRNO=1")
        .arg(format!("-DCMAKE_SYSTEM_PROCESSOR={}", arch))
        .arg("-G")
        .arg("Ninja")
        .arg("-B")
        .arg(&cmake_build);

    let status = proc.status().unwrap();
    assert!(status.success());

    let mut proc = std::process::Command::new("ninja");
    proc.current_dir(&cmake_build).stdout(stderr());

    let status = proc.status().unwrap();
    assert!(status.success());
    let target = std::env::var("TARGET").unwrap();

    let mut proc = std::process::Command::new("../../../toolchain/install/bin/bindgen");
    proc.stdout(stderr())
        .arg("src/lwext4.h")
        .arg("-o")
        .arg("src/lwext4.rs")
        .arg("--")
        .arg(format!("-I{}/cmake-build/include", outdir))
        .arg("-Ilwext4/include")
        .arg(format!(
            "-I../../../toolchain/install/sysroots/{}/include",
            target
        ))
        .args(cflags.split_whitespace());
    eprintln!("running bindgen : {:?}", proc);

    let status = proc.status().unwrap();
    assert!(status.success());

    println!("cargo::rerun-if-changed=src/lwext4.h");
    // Without these, editing the vendored C sources or config headers does not rerun this
    // script, so cmake/ninja never run and the crate silently links a stale liblwext4.a.
    println!("cargo::rerun-if-changed=lwext4/src");
    println!("cargo::rerun-if-changed=lwext4/include");
    println!("cargo::rustc-link-lib=c");
    println!("cargo::rustc-link-search={}/cmake-build/src/", outdir);
    println!("cargo::rustc-link-lib=lwext4");
}
