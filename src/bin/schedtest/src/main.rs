use std::{
    io::Write,
    time::{Duration, Instant},
};

use rand::seq::SliceRandom;

#[allow(dead_code)]
fn jointest() {
    let threads = (0..4).into_iter().map(|j| {
        std::thread::spawn(move || {
            const MAX_TRIALS: u32 = 40;
            for i in 0..MAX_TRIALS {
                println!("trial {}/{}", i * j, MAX_TRIALS * j);
                let mut threads = (0..4)
                    .into_iter()
                    .map(|_| {
                        std::thread::spawn(move || {
                            let delay = rand::random::<u32>() % 100_000;
                            if delay > 10_000 {
                                std::thread::sleep(Duration::from_nanos(delay as u64));
                            }
                        })
                    })
                    .collect::<Vec<_>>();
                threads.shuffle(&mut rand::rng());
                for th in threads {
                    th.join().unwrap();
                }
            }
        })
    });
    for th in threads {
        th.join().unwrap();
    }
}

fn main() {
    //jointest();
    //return;
    let threads = (0..4)
        .into_iter()
        .map(|i| std::thread::spawn(move || thread_main(i)))
        .collect::<Vec<_>>();
    for th in threads {
        th.join().unwrap();
    }
    println!()
}

fn thread_main(num: u32) {
    let start = Instant::now();
    for _n in 0..5 {
        let mut sum = 0;
        for i in 0..1_000_000_000 {
            sum += i;
            if i % 10_000_000 == 0 && false {
                unsafe {
                    print!("{}", char::from_u32_unchecked(b'a' as u32 + num));
                }
                std::io::stdout().flush().unwrap();
            }
            sum = std::hint::black_box(sum);
        }
        std::hint::black_box(sum);
    }
    println!(
        "thread {} in {}ms",
        num,
        (Instant::now() - start).as_millis()
    );
}
