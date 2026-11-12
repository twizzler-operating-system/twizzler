fn main() {
    println!("cargo:rerun-if-changed=csrc");
    let c = cc::Build::new().file("csrc/heap_c.c").compile_intermediates();
    for obj in c {
        println!("cargo:rustc-link-arg-bin=heap-test-c={}", obj.display());
    }
}
