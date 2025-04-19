use hex_literal::hex;
use twizzler_abi::object::Protections;
use twizzler_security::{Cap, Gates, ObjectId, SigningScheme, VerifyingKey};

pub fn rand_32() -> [u8; 32] {
    let mut dest = [0 as u8; 32];
    getrandom::getrandom(&mut dest).unwrap();
    dest
}

#[test]
fn creation_and_verification() {
    let accessor_id: ObjectId = 12345689;
    let target_id: ObjectId = 987654321;
    //https://datatracker.ietf.org/doc/html/rfc6979#appendix-A.2.5
    let target_priv_key = rand_32();
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
        Gates::default(),
    );

    let verifying_key = VerifyingKey::new(SigningScheme::Ecdsa, &target_priv_key).unwrap();

    target_rw_cap
        .unwrap()
        .verify_sig(verifying_key)
        .expect("should be verified ");
}
