mod hash;

use hash::BadHasher;
use kms::KeyManagementScheme;
use lethe::Lethe;
use rand::SeedableRng;
use rand_chacha::ChaChaRng;

fn main() {
    let keyid @ (objid, blkid) = (0, 0);
    let rng = ChaChaRng::seed_from_u64(0);
    let mut lethe: Lethe<ChaChaRng, BadHasher<32>, 32> = Lethe::new(rng);

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
