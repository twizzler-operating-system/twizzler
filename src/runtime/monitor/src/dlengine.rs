use dynlink::{
    compartment::MONITOR_COMPARTMENT_ID,
    engines::{Backing, ContextEngine},
    library::UnloadedLibrary,
    DynlinkError, DynlinkErrorKind,
};
use twizzler_runtime_api::{MapFlags, ObjID};

pub struct Engine;

impl ContextEngine for Engine {
    fn load_segments(
        &mut self,
        src: &Backing,
        ld: &[dynlink::engines::LoadDirective],
    ) -> Result<Vec<Backing>, dynlink::DynlinkError> {
        dynlink::engines::twizzler::load_segments(src, ld)
    }

    fn load_object(&mut self, unlib: &UnloadedLibrary) -> Result<Backing, DynlinkError> {
        let id = name_resolver(&unlib.name)?;
        Ok(Backing::new(
            twizzler_runtime_api::get_runtime()
                .map_object(id, MapFlags::READ)
                .map_err(|_err| DynlinkErrorKind::NewBackingFail)?,
        ))
    }

    fn select_compartment(
        &mut self,
        _unlib: &UnloadedLibrary,
    ) -> Option<dynlink::compartment::CompartmentId> {
        Some(MONITOR_COMPARTMENT_ID)
    }
}

fn name_resolver(mut name: &str) -> Result<ObjID, DynlinkError> {
    if name.starts_with("libstd") {
        name = "libstd.so";
    }
    crate::find_init_name(name).ok_or(
        DynlinkErrorKind::NameNotFound {
            name: name.to_string(),
        }
        .into(),
    )
}
