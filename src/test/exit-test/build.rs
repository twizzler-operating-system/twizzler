fn main() {
    println!("cargo:rerun-if-changed=csrc");
    let c = cc::Build::new()
        .file("csrc/exit_c.c")
        .compile_intermediates();
    for obj in c {
        println!("cargo:rustc-link-arg-bin=exit-test-c={}", obj.display());
    }
    let cxx = cc::Build::new()
        .cpp(true)
        .std("c++17")
        .file("csrc/exit_cxx.cpp")
        .compile_intermediates();
    for obj in cxx {
        println!("cargo:rustc-link-arg-bin=exit-test-cxx={}", obj.display());
    }
    for lib in ["-lc++", "-lc++abi", "-lunwind"] {
        println!("cargo:rustc-link-arg-bin=exit-test-cxx={lib}");
    }
}
