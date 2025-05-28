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
#[allow(unused_imports)]
mod tests {

    use super::*;

    extern crate test;

    use twizzler::object::Object;
    use twizzler_abi::{object::Protections, syscall::ObjectCreate};

    use super::VerifyingKey;
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

        SigningKey::new_keypair(&SigningScheme::Ecdsa, object_create_spec)
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

        let (s_obj, _) = create_default_key_pair();

        let message = "deadbeef".as_bytes();

        s_obj
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
}
