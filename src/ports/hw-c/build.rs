fn main() {
    cc::Build::new().file("src/hw.c").compile("hw");
    println!("cargo::rustc-link-lib=c");
}
