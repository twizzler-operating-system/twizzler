use std::io::stderr;

#[allow(unreachable_code, unused)]
fn main() {
    let outdir = std::env::var("OUT_DIR").unwrap();
    let target = std::env::var("TARGET").unwrap();
    let cflags = std::env::var("CFLAGS").unwrap_or("".to_owned());
    let arch = target.split("-").next().unwrap();
    let cmake_build = format!("{}/cmake-build", outdir);

    // A/B ARM: lwext4's block cache, in blocks (4 KiB each). See BLOCK_DEV_CACHE_SIZE below.
    //
    // This used to be passed here, in CFLAGS, alongside a comment claiming the value was 1024.
    // It never reached the compiler: lwext4's own CMakeLists does
    // `add_definitions(-DCONFIG_BLOCK_DEV_CACHE_SIZE=16)` in the LIB_ONLY branch, which lands on
    // the compile line after CMAKE_C_FLAGS and wins -- so every build has run with a 16-block,
    // 64 KiB cache, and the header's `#ifndef ... 1024` default never applied either. Verified by
    // grepping the generated flags: 21 occurrences of `=16`, none of the intended value. Any A/B
    // run through the old knob varied nothing, which is the likeliest reading of the 08-27
    // "block reads never plateau" note this comment used to carry.
    let cflags = format!("{} -DCONFIG_USE_DEFAULT_CFG -g", cflags);

    //let _ = std::fs::remove_dir_all(&cmake_build);

    let mut proc = std::process::Command::new("cmake");
    proc.current_dir("lwext4")
        .stdout(stderr())
        .arg("-DCMAKE_BUILD_TYPE=Release")
        .arg("-DCMAKE_POLICY_VERSION_MINIMUM=3.5")
        .arg("-DCMAKE_SYSTEM_NAME=Generic")
        .arg("-DLIB_ONLY=True")
        .arg("-DCONFIG_HAVE_OWN_ERRNO=1")
        // A cmake cache variable, so it actually reaches the compile *and* so changing it
        // reconfigures -- the `remove_dir_all(&cmake_build)` below is commented out, and a warm
        // build directory would otherwise silently rebuild nothing and report the old value.
        .arg("-DBLOCK_DEV_CACHE_SIZE=8192")
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
