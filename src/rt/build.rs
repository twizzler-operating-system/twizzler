fn main() {
    if let Ok(target) = std::env::var("TARGET") {
        if let Ok(profile) = std::env::var("PROFILE") {
            println!(
                "cargo::rustc-link-search=target/dynamic/{}/{}",
                target, profile
            );
        }
    }
}
