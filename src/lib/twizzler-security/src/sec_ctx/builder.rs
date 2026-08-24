use alloc::vec::Vec;

use twizzler_abi::{
    object::{ObjID, Protections},
    syscall::{BackingType, LifetimeType, ObjectCreate, ObjectCreateFlags},
};
use twizzler_rt_abi::error::TwzError;

use super::{Mask, SecCtx, SecCtxFlags};
use crate::{Cap, Del};

/// A builder for constructing a [`SecCtx`].
#[derive(Debug, Clone)]
pub struct SecCtxBuilder {
    spec: ObjectCreate,
    global_mask: Protections,
    flags: SecCtxFlags,
    caps: Vec<Cap>,
    dels: Vec<Del>,
    masks: Vec<Mask>,
}

impl Default for SecCtxBuilder {
    fn default() -> Self {
        Self {
            spec: ObjectCreate::new(
                BackingType::Normal,
                LifetimeType::Volatile,
                None,
                ObjectCreateFlags::empty(),
                Protections::all(),
            ),
            global_mask: Protections::all(),
            flags: SecCtxFlags::empty(),
            caps: Vec::new(),
            dels: Vec::new(),
            masks: Vec::new(),
        }
    }
}

impl SecCtxBuilder {
    /// Create a new `SecCtxBuilder` with default object-creation and security-context
    /// settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the complete object create specification.
    pub fn object_create_spec(mut self, spec: ObjectCreate) -> Self {
        self.spec = spec;
        self
    }

    /// Set the verifying key of this `Security Context`.
    pub fn verifying_key(mut self, id: ObjID) -> Self {
        self.spec.kuid = id;
        self
    }

    /// The default protections to access this security context.
    pub fn default_protections(mut self, prots: Protections) -> Self {
        self.spec.def_prot = prots;
        self
    }

    /// Backing storage type for the underlying object.
    pub fn backing_type(mut self, bt: BackingType) -> Self {
        self.spec.bt = bt;
        self
    }

    /// Object lifetime — volatile vs. persistent.
    pub fn lifetime(mut self, lt: LifetimeType) -> Self {
        self.spec.lt = lt;
        self
    }

    /// Low-level object creation flags.
    pub fn object_create_flags(mut self, flags: ObjectCreateFlags) -> Self {
        self.spec.flags = flags;
        self
    }

    /// The security-context-level mask applied to protections granted by this context's
    /// capabilities/delegations.
    pub fn global_mask(mut self, mask: Protections) -> Self {
        self.global_mask = mask;
        self
    }

    /// Flags for this `SecCtx`.
    pub fn flags(mut self, flags: SecCtxFlags) -> Self {
        self.flags = flags;
        self
    }

    /// Convenience for setting `SecCtxFlags::UNDETACHABLE`.
    /// Makes the security context undetachable once attached.
    pub fn undetachable(mut self) -> Self {
        self.flags |= SecCtxFlags::UNDETACHABLE;
        self
    }

    /// Stage a `Cap` to be inserted immediately after the context object is created.
    pub fn cap(mut self, cap: Cap) -> Self {
        self.caps.push(cap);
        self
    }

    /// Stage a `Del` to be inserted immediately after the context object is created.
    pub fn del(mut self, del: Del) -> Self {
        self.dels.push(del);
        self
    }

    /// Stage a `Mask` to be inserted immediately after the context object is created.
    pub fn mask(mut self, mask: Mask) -> Self {
        self.masks.push(mask);
        self
    }

    /// Build the `SecCtx`, creating its backing object and inserting every staged
    /// `Cap`/`Del`/`Mask`, in that order.
    pub fn build(self) -> Result<SecCtx, TwzError> {
        let mut ctx = SecCtx::new(self.spec, self.global_mask, self.flags)?;
        for cap in self.caps {
            ctx.insert_cap(cap)?;
        }
        for del in self.dels {
            ctx.insert_del(del)?;
        }
        for mask in self.masks {
            ctx.insert_mask(mask)?;
        }
        Ok(ctx)
    }
}
