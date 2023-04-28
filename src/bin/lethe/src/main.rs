mod hash;
use hash::BadHasher;

mod rng;
use rng::BadRng;

use kms::KeyManagementScheme;
use lethe::Lethe;

fn main() {
    let keyid @ (objid, blkid) = (0, 0);
    let mut lethe: Lethe<BadRng, BadHasher<32>, 32> = Lethe::new(BadRng::new());

    println!(
        "[derive] objid={objid}, blkid={blkid} => {}",
        hex::encode(lethe.derive(keyid).unwrap())
    );

    println!(
        "[update] objid={objid}, blkid={blkid} => {}",
        hex::encode(lethe.update(keyid).unwrap())
    );

    println!("[commit] => {:?}", lethe.commit());

    println!(
        "[derive] objid={objid}, blkid={blkid} => {}",
        hex::encode(lethe.derive(keyid).unwrap())
    );
}
