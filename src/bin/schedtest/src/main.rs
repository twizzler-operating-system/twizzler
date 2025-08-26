fn main() {
    let threads = (0..4)
        .into_iter()
        .map(|i| std::thread::spawn(move || thread_main(i)))
        .collect::<Vec<_>>();
    for th in threads {
        th.join().unwrap();
    }
}

fn thread_main(num: u32) {
    for _n in 0..100 {
        let mut sum = 0;
        for i in 0..1_000_000_000 {
            sum += i;
            if i % 100_000_000 == 0 {
                print!("{}", num);
            }
            sum = std::hint::black_box(sum);
        }
        std::hint::black_box(sum);
    }
}
