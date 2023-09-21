use lethe_cli::fs::LetheFute;

fn main() {
    let mut x = LetheFute::new().expect("Whatever");

    let buf = &mut [0u8; 100];
    let write_buf = [67u8; 4096];

    x.create("hi");
    x.write("hi", &write_buf, 2075);

    x.read("hi", buf, 2000);
    println!("{}", String::from_utf8(buf.to_vec()).unwrap());
    //x.consolidate();
}