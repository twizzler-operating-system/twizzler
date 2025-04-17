fn main() {
    cc::Build::new().flag("-v").file("src/hw.c").compile("hw");
}
