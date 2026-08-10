#[cfg(feature = "log")]
use log::debug;
use sha2::{Digest, Sha256};
use twizzler_abi::object::{ObjID, Protections};

use crate::{
    CtxMapItem, Gate, HashingAlgo, Revoc, SecFlags, SecurityError, Signature, SigningKey,
    VerifyingKey,
};

#[expect(dead_code)]
/// A Delegation, which can be used to delegate capabilities into other security contexts.
///
/// a Delegation does not own the right it grants: `inner` is a reference to a
/// `Capability or another Delegation stored at a byte offset inside the `provider` security
/// context.
///
/// We resolve a Delegation by following
/// the `inner` field and recursively verifying whatever is actually
/// stored there. That recursion is bounded by `MAX_DELEGATION_NEST`.
#[derive(Debug)]
pub struct Del {
    /// The receiver of this delegation, "The ID of the security context"
    /// If `None`, this delegaiton is valid for any `Security Context` it finds itself in.
    pub receiver: Option<ObjID>,
    /// The provider of this delegation
    pub provider: ObjID,
    /// The object this delegation (transitively) grants access to. Must match the
    /// `target` of whatever `inner` resolves to.
    pub target: ObjID,

    // The mask applied to protections granted by the inner delegation/capability
    pub prot_mask: Protections,

    flags: SecFlags,
    /// The gatemask, read about this in the paper
    gatemask: Gate,
    /// When this delegation is revoked
    revocation: Revoc,

    /// The signature for this delegation
    sig: Signature,

    /// Where the delegated `Cap`/`Del` lives inside `provider`'s security context object.
    pub inner: CtxMapItem,
}

/// The maximum nesting of a delegation chain, can be changed as needed.
pub const MAX_DELEGATION_NEST: usize = 4;

const DEL_SERIALIZED_LEN: usize = 101;

impl Del {
    pub fn new(
        receiver: Option<ObjID>,
        provider: ObjID,
        target: ObjID,
        inner: CtxMapItem,
        revocation: Revoc,
        prot_mask: Protections,
        gates: Gate,
        hashing_algo: HashingAlgo,
        target_priv_key: &SigningKey,
    ) -> Result<Self, SecurityError> {
        #[cfg(feature = "user")]
        check_nest_depth(provider, inner)?;

        let flags: SecFlags = hashing_algo.into();

        #[cfg(feature = "log")]
        debug!(
            "Using flags: {} to create delegation for receiver: {:?}",
            flags, receiver
        );

        let hash_arr = Del::serialize(
            receiver, provider, target, inner, flags, revocation, prot_mask, gates,
        );

        let sig = match hashing_algo {
            HashingAlgo::Blake3 => {
                let hash = blake3::hash(&hash_arr);
                target_priv_key.sign(hash.as_bytes())?
            }
            HashingAlgo::Sha256 => {
                let mut hasher = Sha256::new();
                hasher.update(hash_arr);
                let hash = hasher.finalize();
                target_priv_key.sign(hash.as_slice())?
            }
        };

        Ok(Del {
            receiver,
            provider,
            target,
            prot_mask,
            flags,
            gatemask: gates,
            revocation,
            sig,
            inner,
        })
    }

    /// Verifies the signature inside this delegation
    pub fn verify_sig(&self, verifying_key: &VerifyingKey) -> Result<(), SecurityError> {
        let hash_arr = Self::serialize(
            self.receiver,
            self.provider,
            self.target,
            self.inner,
            self.flags,
            self.revocation,
            self.prot_mask,
            self.gatemask,
        );

        let hash_algo: HashingAlgo = self.flags.try_into()?;

        match hash_algo {
            HashingAlgo::Blake3 => {
                let hash = blake3::hash(&hash_arr);
                verifying_key.verify(hash.as_bytes(), &self.sig)
            }
            HashingAlgo::Sha256 => {
                let mut hasher = Sha256::new();
                hasher.update(&hash_arr);
                let result = hasher.finalize();
                verifying_key.verify(result.as_slice(), &self.sig)
            }
        }
    }

    /// Returns all contents other than sig as a buffer ready to hash
    fn serialize(
        receiver: Option<ObjID>,
        provider: ObjID,
        target: ObjID,
        inner: CtxMapItem,
        flags: SecFlags,
        revocation: Revoc,
        prot_mask: Protections,
        gates: Gate,
    ) -> [u8; DEL_SERIALIZED_LEN] {
        let mut hash_arr: [u8; DEL_SERIALIZED_LEN] = [0; DEL_SERIALIZED_LEN];
        hash_arr[0..16].copy_from_slice(
            &receiver
                .map(|obj_id| obj_id.raw().to_le_bytes())
                .unwrap_or([0; 16]),
        );
        hash_arr[16..32].copy_from_slice(&provider.raw().to_le_bytes());
        hash_arr[32..48].copy_from_slice(&target.raw().to_le_bytes());
        hash_arr[48..50].copy_from_slice(&flags.bits().to_le_bytes());
        hash_arr[50..66].copy_from_slice(&revocation.to_bytes());
        hash_arr[66..74].copy_from_slice(&gates.offset.to_le_bytes());
        hash_arr[74..82].copy_from_slice(&gates.length.to_le_bytes());
        hash_arr[82..90].copy_from_slice(&gates.align.to_le_bytes());
        hash_arr[90..92].copy_from_slice(&prot_mask.bits().to_le_bytes());
        hash_arr[92] = inner.item_type as u8;
        hash_arr[93..101].copy_from_slice(&(inner.offset as u64).to_le_bytes());
        hash_arr
    }
}

/// Walks `inner` through `provider` objects, to check that the chain bottoms out
/// at a `Cap` within `MAX_DELEGATION_NEST` hops.
///
/// This is mainly to warn a user if a delegation they are trying to create would exceed the nesting
/// depth.
#[cfg(feature = "user")]
fn check_nest_depth(provider: ObjID, inner: CtxMapItem) -> Result<(), SecurityError> {
    use twizzler::object::{MapFlags, Object, RawObject};

    use crate::Cap;

    let mut provider = provider;
    let mut item_type = inner.item_type;
    let mut offset = inner.offset;

    for _ in 0..MAX_DELEGATION_NEST {
        let obj = Object::<()>::map(provider, MapFlags::READ)
            .map_err(|_| SecurityError::MaxNestExceeded)?;

        match item_type {
            crate::CtxMapItemType::Cap => {
                obj.lea(offset, size_of::<Cap>())
                    .ok_or(SecurityError::MaxNestExceeded)?;
                return Ok(());
            }
            crate::CtxMapItemType::Del => {
                let ptr = obj
                    .lea(offset, size_of::<Del>())
                    .ok_or(SecurityError::MaxNestExceeded)?
                    .cast::<Del>();

                // SAFETY: `provider` is mapped above and `ptr` points `size_of::<Del>()`
                // bytes into it.
                let next = unsafe { &*ptr };

                provider = next.provider;
                item_type = next.inner.item_type;
                offset = next.inner.offset;
            }
        }
    }

    Err(SecurityError::MaxNestExceeded)
}

#[cfg(test)]
#[cfg(feature = "user")]
#[allow(unused_imports)]
mod tests {

    use super::*;
    use crate::*;
    extern crate test;
    use twizzler::object::TypedObject;
    use twizzler_abi::{object::Protections, syscall::ObjectCreate};

    /// Insert a fresh `Cap` for `target` into `ctx` and hand back the `CtxMapItem` that
    /// refers to it, ready to be used as a `Del`'s `inner`.
    fn insert_default_capability(
        ctx: &mut SecCtx,
        target: ObjID,
        s_key: &SigningKey,
    ) -> CtxMapItem {
        let cap = Cap::new(
            target,
            0x321.into(),
            Protections::all(),
            s_key,
            Revoc::default(),
            Gate::default(),
            HashingAlgo::Sha256,
        )
        .expect("Capability should have been created.");

        ctx.insert_cap(cap)
            .expect("capability should have been inserted");

        *ctx.base()
            .map
            .get(&target)
            .expect("capability should be present in the map")
            .last()
            .expect("capability entry should be present")
    }

    /// Create a default delegation, referencing a capability in `ctx`.
    fn default_delegation(ctx: &mut SecCtx, target: ObjID, s_key: &SigningKey) -> Del {
        let inner = insert_default_capability(ctx, target, s_key);

        Del::new(
            Some(0x111.into()),
            ctx.id(),
            target,
            inner,
            Revoc::default(),
            Protections::all(),
            Gate::default(),
            HashingAlgo::Sha256,
            s_key,
        )
        .expect("Delegation should have been created.")
    }

    #[test]
    fn test_delegation_creation() {
        let (s, _v) = SigningKey::new_keypair(&SigningScheme::Ecdsa, ObjectCreate::default())
            .expect("keypair creation should not have errored!");

        let mut ctx = SecCtx::default();
        let _del = default_delegation(&mut ctx, 0x123.into(), s.base());
    }

    #[test]
    fn test_delegation_verification() {
        let (s, v) = SigningKey::new_keypair(&SigningScheme::Ecdsa, ObjectCreate::default())
            .expect("keypair creation should not have errored!");

        let mut ctx = SecCtx::default();
        let del = default_delegation(&mut ctx, 0x123.into(), s.base());

        del.verify_sig(v.base())
            .expect("delegation should have been verified.")
    }

    #[test]
    fn test_delegation_nest_limit() {
        let (s, _v) = SigningKey::new_keypair(&SigningScheme::Ecdsa, ObjectCreate::default())
            .expect("keypair creation should not have errored!");

        let mut ctx = SecCtx::default();
        let target: ObjID = 0x123.into();
        let receiver: ObjID = 0x111.into();

        let mut inner = insert_default_capability(&mut ctx, target, s.base());

        // Building a chain up to MAX_DELEGATION_NEST hops deep must succeed at every step,
        // since each hop is still within the bound once it's created.
        for hop in 0..MAX_DELEGATION_NEST {
            let del = Del::new(
                Some(receiver),
                ctx.id(),
                target,
                inner,
                Revoc::default(),
                Protections::all(),
                Gate::default(),
                HashingAlgo::Sha256,
                s.base(),
            )
            .unwrap_or_else(|e| panic!("delegation at hop {hop} should have been created: {e:?}"));

            ctx.insert_del(del)
                .expect("delegation should have been inserted");

            inner = *ctx
                .base()
                .map
                .get(&target)
                .expect("delegation should be present in the map")
                .last()
                .expect("delegation entry should be present");
        }

        // One more hop pushes the chain past MAX_DELEGATION_NEST and must be rejected.
        match Del::new(
            Some(receiver),
            ctx.id(),
            target,
            inner,
            Revoc::default(),
            Protections::all(),
            Gate::default(),
            HashingAlgo::Sha256,
            s.base(),
        ) {
            Err(SecurityError::MaxNestExceeded) => {}
            other => panic!("expected MaxNestExceeded, got {other:?}"),
        }
    }
}
