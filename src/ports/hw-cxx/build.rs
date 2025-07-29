fn main() {
    cc::Build::new()
        .cpp(true)
        .compiler("clang++")
        .file("src/test.cpp")
        .cpp_set_stdlib("c++")
        .compile("cxxtest");

    println!("cargo::rustc-link-lib=c");
    println!("cargo::rustc-link-lib=static=c++abi");
    println!("cargo::rustc-link-lib=unwind");
}
