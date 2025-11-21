use twizzler::{
    error::TwzError,
    marker::{BaseType, StoreCopy},
    object::{ObjID, Object, ObjectBuilder, RawObject},
};
use twizzler_abi::{object::Protections, syscall::sys_thread_active_sctx_id};

use super::SecCtx;
use crate::{Cap, Revoc, SigningKey};

pub trait SecureBuilderExt<Base: BaseType + StoreCopy> {
    fn build_secure(
        &self,
        base: Base,
        s_key: &SigningKey,
        ctx: Option<ObjID>,
    ) -> Result<Object<Base>, TwzError>;
}

impl<Base> SecureBuilderExt<Base> for ObjectBuilder<Base>
where
    Base: BaseType + StoreCopy,
{
    fn build_secure(
        &self,
        base: Base,
        s_key: &SigningKey,
        //NOTE: once default global masks get fixed to all prots, remove this argument,use
        // currently attached ctx always, caller can choose what to attach to.
        ctx: Option<ObjID>,
    ) -> Result<Object<Base>, TwzError> {
        self.build_inplace(|tx| {
            let mut sec_ctx = ctx
                .map(|id| {
                    // we need to attach to the context they provided
                    let ctx = SecCtx::try_from(id)?;
                    ctx.set_active().unwrap();
                    Ok::<SecCtx, TwzError>(ctx)
                })
                .unwrap_or(Ok(SecCtx::attached_ctx()))?;

            let cap = Cap::new(
                tx.id(),
                sec_ctx.id(),
                Protections::READ | Protections::WRITE,
                s_key,
                Default::default(),
                Default::default(),
                Default::default(),
            )?;

            sec_ctx.insert_cap(cap)?;

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
            .build_secure(base, s_key, None)
            .expect("should have built successfully");

        // our current thing should be able to read this just fine
        let base_ptr = obj.base_ptr::<MessageStoreObj>();

        unsafe {
            let base = *base_ptr;
            assert!(base.message == "Hello")
        }
    }
}
