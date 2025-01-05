use hex_literal::hex;
use twizsec::{Cap, ObjectId, Permissions, SigningScheme, VerifyingKey};

#[test]
fn creation_and_verification() {
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
        Permissions::READ | Permissions::WRITE,
        target_priv_key,
    );

    let verifying_key = VerifyingKey::new(SigningScheme::Ecdsa, &target_priv_key).unwrap();

    target_rw_cap
        .unwrap()
        .verify_sig(verifying_key)
        .expect("should be verified ");
}
