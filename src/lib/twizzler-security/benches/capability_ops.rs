use std::hint::black_box;

use criterion::{criterion_group, criterion_main, Criterion};
use hex_literal::hex;
use twizzler_abi::object::Protections;
use twizzler_security::{Cap, ObjectId, Permissions, SigningScheme, VerifyingKey};

// use cargo bench for these

fn verify_bench(c: &mut Criterion) {
    let accessor_id: ObjectId = 12345689;
    let target_id: ObjectId = 987654321;
    //https://datatracker.ietf.org/doc/html/rfc6979#appendix-A.2.5
    let target_priv_key = hex!("C9AFA9D845BA75166B5C215767B1D6934E50C3DB36E89B127B8A622B120F6721");
    // basically this priv_key needs to be 32 bytes long, if we want the keys to be more adaptable,
    // we would need a key struct and abstract it away, since right now the implementation only
    // works if we use a hard-coded size

    // now lets say accessor wants to reach target
    let target_rw_cap = Cap::new(
        target_id,
        accessor_id,
        Protections::all(),
        target_priv_key,
        Default::default(),
        Default::default(),
    )
    .unwrap();

    let verifying_key = VerifyingKey::new(SigningScheme::Ecdsa, &target_priv_key).unwrap();

    c.bench_function("Verifying SHA256 and P256 ECDSA Signature", |b| {
        b.iter(|| target_rw_cap.verify_sig(black_box(verifying_key)))
    });
}

fn creation_bench(c: &mut Criterion) {
    let accessor_id: ObjectId = 12345689;
    let target_id: ObjectId = 987654321;
    //https://datatracker.ietf.org/doc/html/rfc6979#appendix-A.2.5
    let target_priv_key = hex!("C9AFA9D845BA75166B5C215767B1D6934E50C3DB36E89B127B8A622B120F6721");
    // basically this priv_key needs to be 32 bytes long, if we want the keys to be more adaptable,
    // we would need a key struct and abstract it away, since right now the implementation only
    // works if we use a hard-coded size

    // now lets say accessor wants to reach target
    c.bench_function(
        "Creating Capability with SHA256 and P256 ECDSA Signature",
        |b| {
            b.iter(|| {
                Cap::new(
                    black_box(target_id),
                    black_box(accessor_id),
                    black_box(Permissions::READ | Permissions::WRITE),
                    black_box(target_priv_key),
                    black_box(Default::default()),
                    black_box(Default::default()),
                )
            })
        },
    );
}

criterion_group!(benches, creation_bench, verify_bench);
criterion_main!(benches);
