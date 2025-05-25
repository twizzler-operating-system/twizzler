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
    use core::hint::black_box;

    use super::*;

    extern crate test;

    use test::Bencher;
    use twizzler::{
        marker::BaseType,
        object::{Object, ObjectBuilder},
    };
    use twizzler_abi::{object::Protections, syscall::ObjectCreate};
    use twizzler_rt_abi::error::TwzError;

    use super::{Signature, VerifyingKey, MAX_KEY_SIZE};
    use crate::{SecurityError, SigningScheme};

    #[test]
    fn test_key_creation() {
        let object_create_spec = ObjectCreate::new(
            Default::default(),
            twizzler_abi::syscall::LifetimeType::Persistent,
            Default::default(),
            Default::default(),
            Protections::all(),
        );
        let (skey, vkey) = SigningKey::new_keypair(&SigningScheme::Ecdsa, object_create_spec)
            .expect("keys should be generated properly");
    }

    fn create_default_key_pair() -> (Object<SigningKey>, Object<VerifyingKey>) {
        let object_create_spec = ObjectCreate::new(
            Default::default(),
            twizzler_abi::syscall::LifetimeType::Persistent,
            Default::default(),
            Default::default(),
            Protections::all(),
        );

        SigningKey::new_keypair(&SigningScheme::Ecdsa, object_create_spec)
            .expect("Keys should be generated properly")
    }

    #[test]
    fn test_signing() {
        use twizzler::object::TypedObject;

        let (s_obj, v_obj) = create_default_key_pair();

        let message = "deadbeef".as_bytes();

        let sig = s_obj
            .base()
            .sign(message)
            .expect("Signature should succeed");
    }

    #[test]
    fn test_verifying() {
        use twizzler::object::TypedObject;

        let (s_obj, v_obj) = create_default_key_pair();

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
    // #[bench]
    // fn bench_keypair_creation(b: &mut Bencher) {
    //     let object_create_spec = ObjectCreate::new(
    //         Default::default(),
    //         twizzler_abi::syscall::LifetimeType::Persistent,
    //         Default::default(),
    //         Default::default(),
    //         Protections::all(),
    //     );

    //     b.iter(|| {
    //         let (s_obj, v_obj) = SigningKey::new_keypair(&SigningScheme::Ecdsa,
    // object_create_spec)             .expect("Keys should be generated properly");
    //     });
    // }

    #[bench]
    fn bench_something_else(b: &mut Bencher) {
        b.tier(|| {
            let x = black_box(5 * 10);
        })
    }
}
