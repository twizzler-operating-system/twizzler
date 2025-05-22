mod sig;
mod sign;
mod verify;
pub use sig::*;
pub use sign::*;
pub use verify::*;

const MAX_KEY_SIZE: usize = 128;

// currently these tests can only run in user space, would have to write their own
// tests written inside kernel to run.
#[cfg(feature = "user")]
mod tests {
    use super::*;

    extern crate test;

    use test::Bencher;

    #[test]
    fn test_key_creation() {
        let object_create_spec = ObjectCreate::new(
            Default::default(),
            twizzler_abi::syscall::LifetimeType::Persistent,
            Default::default(),
            Default::default(),
        );
        let (skey, vkey) = SigningKey::new_keypair(&SigningScheme::Ecdsa, object_create_spec)
            .expect("keys should be generated properly");
    }

    #[test]
    fn test_signing() {
        use twizzler::object::TypedObject;

        let object_create_spec = ObjectCreate::new(
            Default::default(),
            twizzler_abi::syscall::LifetimeType::Persistent,
            Default::default(),
            Default::default(),
        );

        let (s_obj, v_obj) = SigningKey::new_keypair(&SigningScheme::Ecdsa, object_create_spec)
            .expect("Keys should be generated properly");
        let message = "deadbeef".as_bytes();

        let sig = s_obj
            .base()
            .sign(message)
            .expect("Signature should succeed");
    }

    #[test]
    fn test_verifying() {
        use twizzler::object::TypedObject;

        let object_create_spec = ObjectCreate::new(
            Default::default(),
            twizzler_abi::syscall::LifetimeType::Persistent,
            Default::default(),
            Default::default(),
        );

        let (s_obj, v_obj) = SigningKey::new_keypair(&SigningScheme::Ecdsa, object_create_spec)
            .expect("Keys should be generated properly");
        let message = "deadbeef".as_bytes();

        let sig = s_obj
            .base()
            .sign(message)
            .expect("Signature should succeed");

        v_obj
            .base()
            .verify(message, &sig)
            .expect("Should be verified properly");
    }

    //NOTE: currently we can only bench in user space, need to benchmark this in kernel space as
    // well
    #[bench]
    fn bench_keypair_creation(b: &mut Bencher) {
        let object_create_spec = ObjectCreate::new(
            Default::default(),
            twizzler_abi::syscall::LifetimeType::Persistent,
            Default::default(),
            Default::default(),
        );
        b.iter(|| {
            let (skey, vkey) =
                SigningKey::new_keypair(&SigningScheme::Ecdsa, object_create_spec.clone())
                    .expect("Keys should be generated properly.");
        });
    }
}

