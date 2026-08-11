use std::io::stderr;

#[allow(unreachable_code, unused)]
fn main() {
    let outdir = std::env::var("OUT_DIR").unwrap();
    let target = std::env::var("TARGET").unwrap();
    // CFLAGS is scoped per-target (CFLAGS_<target>) so that it doesn't leak into
    // host-native builds of other crates; fall back to the bare CFLAGS for
    // compatibility with invocations that set it directly.
    let cflags = std::env::var(format!("CFLAGS_{}", target.replace('-', "_")))
        .or_else(|_| std::env::var("CFLAGS"))
        .unwrap_or("".to_owned());
    let arch = target.split("-").next().unwrap();
    let cmake_build = format!("{}/cmake-build", outdir);
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let clang_path = format!("{}/../../../toolchain/install/bin/clang", manifest_dir);

    let cflags = format!("{} -DCONFIG_USE_DEFAULT_CFG", cflags);

    //let _ = std::fs::remove_dir_all(&cmake_build);

    let mut proc = std::process::Command::new("cmake");
    proc.current_dir("lwext4")
        .stdout(stderr())
        // CC/CFLAGS are scoped per-target (CC_<target>/CFLAGS_<target>) upstream, so cmake
        // won't pick up the cross-compile flags from the ambient environment on its own.
        // Pass them explicitly so the toolchain it detects actually targets `target` instead
        // of silently falling back to a host-native compile (which later fails to link
        // against the twizzler-target linker flags).
        .env("CC", &clang_path)
        .env("CFLAGS", &cflags)
        .arg("-DCMAKE_BUILD_TYPE=Release")
        .arg("-DCMAKE_POLICY_VERSION_MINIMUM=3.5")
        .arg("-DCMAKE_SYSTEM_NAME=Generic")
        .arg("-DLIB_ONLY=True")
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

    let bindgen_out = format!("{}/lwext4.rs", outdir);

    let mut proc = std::process::Command::new("../../../toolchain/install/bin/bindgen");
    proc.stdout(stderr())
        .arg("src/lwext4.h")
        .arg("-o")
        .arg(&bindgen_out)
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
    println!("cargo::rustc-link-lib=c");
    println!("cargo::rustc-link-search={}/cmake-build/src/", outdir);
    println!("cargo::rustc-link-lib=lwext4");
}
