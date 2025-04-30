use std::io::stderr;

fn main() {
    cc::Build::new().file("src/hw.c").compile("hw");

    let outdir = std::env::var("OUT_DIR").unwrap();
    let target = std::env::var("TARGET").unwrap();
    let arch = target.split("-").next().unwrap();
    let cmake_build = format!("{}/cmake-build", outdir);

    let _ = std::fs::remove_dir_all(&cmake_build);

    let mut proc = std::process::Command::new("cmake");
    proc.current_dir("lwext4")
        .stdout(stderr())
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

    println!("cargo::rustc-link-lib=c");
}
