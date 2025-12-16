use twizzler::{
    error::TwzError,
    marker::{BaseType, StoreCopy},
    object::{Object, ObjectBuilder, RawObject},
};
use twizzler_abi::object::Protections;

use super::SecCtx;
use crate::{Cap, SigningKey};

/// An extension trait for the ObjectBuilder from the
/// `twizzler` crate that allows for the creation of objects
/// that have restrained default permissions.
///
///
/// We get around
/// the write requirement by creating a capability before
/// we write the base of the object.
pub trait SecureBuilderExt<Base: BaseType + StoreCopy> {
    /// Builds a "secure" object, one without `Protections::READ|Protections::Write` as its
    /// `default_prots`.
    ///
    /// It achieves this by creating a capability for the object within the current security
    /// context, and then writing to the object after that capability has been created.
    fn build_secure(&self, base: Base, s_key: &SigningKey) -> Result<Object<Base>, TwzError>;
}

impl<Base> SecureBuilderExt<Base> for ObjectBuilder<Base>
where
    Base: BaseType + StoreCopy,
{
    fn build_secure(&self, base: Base, s_key: &SigningKey) -> Result<Object<Base>, TwzError> {
        self.build_inplace(|tx| {
            let mut curr_sec_ctx = SecCtx::active_ctx();
            let cap = Cap::new(
                tx.id(),
                curr_sec_ctx.id(),
                Protections::READ | Protections::WRITE,
                s_key,
                Default::default(),
                Default::default(),
                Default::default(),
            )?;

            curr_sec_ctx.insert_cap(cap)?;

            // now we have permissions to write here!
            tx.write(base)
        })
    }
}

#[cfg(feature = "user")]
#[cfg(test)]
mod tests {
    use twizzler::object::{ObjectBuilder, RawObject};

    use crate::SigningKey;

    #[derive(Debug, Clone)]
    struct MessageStoreObj {
        message: heapless::String<256>,
    }

    impl BaseType for MessageStoreObj {
        fn fingerprint() -> u64 {
            11234
        }
    }
    #[test]
    fn build_sealed_object() {
        use super::SecureBuilderExt as _;

        let (s_key, v_key) = SigningKey::new_keypair(&SigningScheme::Ecdsa, Default::default())
            .expect("should have worked");

        let spec = ObjectCreate::new(
            Default::default(),
            Default::default(),
            Some(v_key.id()),
            Default::default(),
            Protections::empty(),
        );

        let base = MessageStoreObj {
            // message: args.message,
            message: heapless::String::<256>::try_from("Hello")
                .expect("message was longer than 256 characters!!"),
        };
        let obj = ObjectBuilder::new(spec)
            .build_secure(base, s_key)
            .expect("should have built successfully");

        // our current thing should be able to read this just fine
        let base_ptr = obj.base_ptr::<MessageStoreObj>();

        unsafe {
            let base = *base_ptr;
            assert!(base.message == "Hello")
        }
    }
}
