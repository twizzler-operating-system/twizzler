fn main() {
    println!("sending request");
    let agent: ureq::Agent = ureq::Agent::config_builder().no_delay(false).build().into();

    // let body: String = ureq::get("http://example.com")
    //     .header("Example-Header", "header value")
    //     .call()
    //     .expect("Failed to communicate with website")
    //     .body_mut()
    //     .read_to_string()
    //     .expect("Failed to communicate with website");
    let body: String = agent
        .get("https://google.com")
        // .get("http://example.com")
        // .header("Example-Header", "header value")
        .call()
        .expect("Failed to communicate with website")
        .body_mut()
        .read_to_string()
        .expect("Failed to communicate with website");

    println!("{body}")
}
