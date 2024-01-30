use std::{alloc::Layout, cell::OnceCell, collections::HashMap, ptr::NonNull};

use dynlink::{compartment::CompartmentId, library::BackingData};
use monitor_api::{SharedCompConfig, TlsTemplateInfo};
use talc::{ErrOnOom, Span, Talc};
use twizzler_abi::{
    object::{MAX_SIZE, NULLPAGE_SIZE},
    syscall::{sys_object_create, BackingType, LifetimeType, ObjectCreate, ObjectCreateFlags},
};
use twizzler_object::ObjID;
use twizzler_runtime_api::{MapFlags, ObjectHandle};
use twz_rt::monitor::RuntimeThreadControl;

/// The monitor's representation of a compartment.
pub struct Comp {
    /// This compartment's security context.
    pub sctx_id: ObjID,
    /// The dynlink ID for this compartment.
    pub compartment_id: CompartmentId,
    /// An object we can use to allocate memory and give to the compartment.
    /// This object is writable by the monitor and read-only to only this compartment.
    pub comp_alloc_obj: ObjectHandle,
    // The allocator for the above object.
    allocator: Talc<ErrOnOom>,
    // The base config data for the compartment, located within the alloc object.
    comp_config: OnceCell<NonNull<SharedCompConfig>>,

    // A map of threads that have entered this compartment and have associated runtime data.
    thread_map: HashMap<ObjID, CompThreadInfo>,

    name: String,
}

/// Safety: this is needed because of the comp_config field, but this points to an object that is
/// same lifetime as Comp, and is unchanging.
unsafe impl Send for Comp {}

impl core::fmt::Debug for Comp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "comp:{}({:x}, {})",
            &self.name, &self.sctx_id, &self.compartment_id
        )
    }
}

impl core::fmt::Display for Comp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "comp:{}", &self.name)
    }
}

pub(crate) fn make_new_comp_alloc_object() -> ObjectHandle {
    // TODO: in the future, we'll need to make this a runtime state object, and limit access rights.
    let id = sys_object_create(
        ObjectCreate::new(
            BackingType::Normal,
            LifetimeType::Volatile,
            None,
            ObjectCreateFlags::empty(),
        ),
        &[],
        &[],
    )
    .unwrap();

    twizzler_runtime_api::get_runtime()
        .map_object(id.as_u128(), MapFlags::READ | MapFlags::WRITE)
        .unwrap()
}

impl Comp {
    /// Construct a new compartment. This can fail, since it allocates memory within the compartment
    /// (for the TLS template and shared config data).
    pub fn new<Backing: BackingData>(
        sctx_id: ObjID,
        compartment: &mut dynlink::compartment::Compartment<Backing>,
    ) -> miette::Result<Self> {
        // First make a new allocation object and initialize it.
        let comp_alloc_obj = make_new_comp_alloc_object();
        let mut allocator = Talc::new(ErrOnOom);
        // Safety: the start and end pointers point within the same object.
        unsafe {
            let start = comp_alloc_obj.start.add(NULLPAGE_SIZE);
            let end = start.add(MAX_SIZE - NULLPAGE_SIZE * 2);
            // Unwrap-Ok: There is enough memory to claim, and the allocator is initialized.
            allocator.claim(Span::new(start, end)).unwrap();
        }

        let mut comp = Self {
            sctx_id,
            compartment_id: compartment.id,
            comp_alloc_obj,
            thread_map: Default::default(),
            allocator,
            comp_config: OnceCell::new(),
            name: compartment.name.clone(),
        };

        // Construct the TLS template.
        let template_info = compartment
            .build_tls_region(RuntimeThreadControl::new(0), |layout| {
                comp.monitor_alloc(layout)
            })?;

        // Init the shared compartment config. We'll leak this TLS template since we are manually
        // managing its lifetime.
        let temp = Box::new(TlsTemplateInfo::from(template_info));
        let temp = Box::leak(temp);
        let cc = comp
            .monitor_new(SharedCompConfig::new(sctx_id, temp))
            .ok_or_else(|| {
                miette::miette!(
                    "failed to allocate shared compartment config data within compartment"
                )
            })?;

        // Unwrap-Ok: this will never try to overwrite, since we are constructing.
        comp.comp_config.set(cc).unwrap();
        Ok(comp)
    }

    /// Create a new allocated region for data.
    pub fn monitor_new<T>(&mut self, data: T) -> Option<NonNull<T>> {
        // Safety: T is Sized, the allocated memory has a layout for T.
        unsafe {
            let mem = self.allocator.malloc(Layout::new::<T>()).ok()?.cast();
            *mem.as_ptr() = data;
            Some(mem)
        }
    }

    /// Raw allocate and zero.
    pub fn monitor_alloc(&mut self, layout: Layout) -> Option<NonNull<u8>> {
        unsafe {
            let mem = self.allocator.malloc(layout).ok()?;
            mem.as_ptr().write_bytes(0, layout.size());
            Some(mem)
        }
    }

    /// Get information about a single thread within this compartment.
    pub fn get_thread_info(&mut self, thid: ObjID) -> &mut CompThreadInfo {
        self.thread_map
            .entry(thid)
            .or_insert_with(|| CompThreadInfo::new(thid))
    }

    /// Get the compartment config.
    pub fn get_comp_config(&self) -> &SharedCompConfig {
        // Safety: this reference is valid as long as self is valid.
        // Unwrap-Ok: we set this during compartment construction.
        unsafe { self.comp_config.get().unwrap().as_ref() }
    }
}

pub struct CompThreadInfo {
    pub thread_id: ObjID,
    pub stack_obj: Option<ObjectHandle>,
    pub thread_ptr: usize,
}

impl CompThreadInfo {
    pub fn new(thread_id: ObjID) -> Self {
        Self {
            thread_id,
            stack_obj: None,
            thread_ptr: 0,
        }
    }
}
