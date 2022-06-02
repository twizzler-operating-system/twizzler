use std::mem::MaybeUninit;

use twizzler_abi::{
    marker::BaseType,
    object::{ObjID, Protections},
    syscall::{
        sys_object_create, BackingType, CreateTieFlags, CreateTieSpec, LifetimeType, ObjectCreate,
        ObjectCreateError, ObjectCreateFlags, ObjectSource,
    },
};

use crate::{
    init::{ObjectInitError, ObjectInitFlags},
    object::Object,
};

/// A builder-pattern type for making a new object.
pub struct CreateSpec {
    lifetime: LifetimeType,
    backing: BackingType,
    kuid: Option<ObjID>,
    flags: ObjectCreateFlags,
    ties: Vec<CreateTieSpec>,
    srcs: Vec<ObjectSource>,
}

impl CreateSpec {
    /// Construct a basic CreateSpec.
    pub fn new(lifetime: LifetimeType, backing: BackingType) -> Self {
        Self {
            ties: vec![],
            srcs: vec![],
            lifetime,
            backing,
            kuid: None,
            flags: ObjectCreateFlags::empty(),
        }
    }

    /// Set the public key ID for this new object.
    pub fn key(&mut self, kuid: ObjID) -> &mut Self {
        self.kuid = Some(kuid);
        self
    }

    /// Add a tie to another object.
    pub fn tie<T>(&mut self, other: &Object<T>, flags: CreateTieFlags) -> &mut Self {
        self.ties.push(CreateTieSpec::new(other.id(), flags));
        self
    }

    /// Add a source for object creation.
    pub fn src<T>(&mut self, src: ObjectSource) -> &mut Self {
        self.srcs.push(src);
        self
    }
}

/// Possible object creation errors.
#[derive(Copy, Clone, Debug)]
pub enum CreateError {
    Create(ObjectCreateError),
    Init(ObjectInitError),
}

impl<T> Object<T> {
    fn raw_create(spec: &CreateSpec) -> Result<ObjID, ObjectCreateError> {
        let oc = ObjectCreate::new(spec.backing, spec.lifetime, spec.kuid, spec.flags);
        sys_object_create(oc, &spec.srcs, &spec.ties)
    }

    /// Create an object, setting up the initial value for the base in a closure.
    pub fn create_with(
        spec: &CreateSpec,
        f: impl FnOnce(&mut Object<MaybeUninit<T>>),
    ) -> Result<Self, CreateError> {
        let id = Self::raw_create(spec).map_err(CreateError::Create)?;
        let mut obj = Object::<MaybeUninit<T>>::init_id(
            id,
            Protections::READ | Protections::WRITE,
            ObjectInitFlags::empty(),
        )
        .map_err(CreateError::Init)?;

        f(&mut obj);
        // TODO: persistence barrier
        // TODO: delete if we fail to map
        Ok(unsafe { core::mem::transmute(obj) })
    }
}

impl<T: BaseType> Object<T> {
    /// Create an object, setting up the initial value for base using the BaseType's init function.
    pub fn create<A>(spec: &CreateSpec, args: A) -> Result<Self, CreateError> {
        let id = Self::raw_create(spec).map_err(CreateError::Create)?;
        let obj = Self::init_id(
            id,
            Protections::READ | Protections::WRITE,
            ObjectInitFlags::empty(),
        )
        .map_err(CreateError::Init)?;
        let base_raw: *mut T = obj.raw_lea_mut(0);
        unsafe {
            base_raw.write(T::init(args));
        }
        // TODO: persistence barrier
        // TODO: delete if we fail to map
        Ok(obj)
    }
}
