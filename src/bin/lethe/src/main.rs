mod hash;
use hash::BadHasher;

mod rng;
use rng::BadRng;

use khf::Khf;
use kms::KeyManagementScheme;

fn main() {
    let mut forest: Khf<BadRng, BadHasher<32>, 32> = Khf::new(BadRng::new(), &[2, 2]);
    println!("{}", hex::encode(forest.derive(0).unwrap()));
}
