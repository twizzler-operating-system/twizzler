fn main() {
    println!("cargo::rustc-link-lib=twz_rt");
    println!("cargo::rustc-link-search=target/dynamic/x86_64-unknown-twizzler/release");
}
