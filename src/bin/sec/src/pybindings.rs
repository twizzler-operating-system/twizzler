//! Python bindings for `twizzler` (object creation) and `twizzler-security`
//! (capabilities/delegation/security contexts), registered into the
//! embedded interpreter started by `sec repl`.
//!
//! Each function here mirrors an existing `sec` CLI code path in
//! `main.rs` one-to-one, operating on hex-encoded `ObjID`s the same way the
//! CLI's flags do, rather than exposing live Rust handles to Python.

use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use twizzler::object::{Object, ObjectBuilder, TypedObject};
use twizzler_abi::{
    object::{ObjID, Protections},
    syscall::{ObjectCreate, sys_sctx_attach, sys_thread_set_active_sctx_id},
};
use twizzler_rt_abi::object::MapFlags;
use twizzler_security::{
    CapBuilder, DelBuilder, SecCtx, SecCtxBuilder, SecCtxFlags, SecureBuilderExt as _, SigningKey,
    SigningScheme,
};

use crate::MessageStoreObj;

fn to_py_err<E: core::fmt::Debug>(e: E) -> PyErr {
    PyRuntimeError::new_err(format!("{e:?}"))
}

fn parse_obj_id(arg: &str) -> PyResult<ObjID> {
    let as_num = u128::from_str_radix(arg, 16).map_err(|e| PyValueError::new_err(e.to_string()))?;
    Ok(ObjID::from(as_num))
}

/// Optionally attach + activate a security context by hex ObjID, mirroring
/// the `executing_ctx` handling repeated in `CtxAddCommands::{Cap,Del}`.
fn maybe_activate(executing_ctx: Option<&str>) -> PyResult<()> {
    if let Some(id) = executing_ctx {
        let sec_ctx = SecCtx::try_from(parse_obj_id(id)?).map_err(to_py_err)?;
        sys_sctx_attach(sec_ctx.id()).map_err(to_py_err)?;
        sys_thread_set_active_sctx_id(sec_ctx.id()).map_err(to_py_err)?;
    }
    Ok(())
}

/// Create a new signing/verifying keypair. Mirrors `Commands::Key(KeyCommands::NewPair)`.
/// Returns `(signing_key_id, verifying_key_id)` as hex strings.
#[pyfunction]
fn new_keypair() -> PyResult<(String, String)> {
    let (s_key, v_key) = SigningKey::new_keypair(&SigningScheme::Ecdsa, Default::default())
        .map_err(to_py_err)?;
    Ok((format!("{:x}", s_key.id()), format!("{:x}", v_key.id())))
}

/// Create a new object with a simple string payload. Mirrors `ObjCommands::New`.
/// Returns the new object's ObjID as a hex string.
#[pyfunction]
fn create_object(verifying_key_id: &str, message: &str) -> PyResult<String> {
    let spec = ObjectCreate::new(
        Default::default(),
        Default::default(),
        Some(parse_obj_id(verifying_key_id)?),
        Default::default(),
        Protections::READ | Protections::WRITE,
    );

    let base = MessageStoreObj::new(message).map_err(PyValueError::new_err)?;

    let obj = ObjectBuilder::new(spec).build(base).map_err(to_py_err)?;
    Ok(format!("{:x}", obj.id()))
}

/// Create a new object with no default permissions (a capability is
/// required to access it). Mirrors `ObjCommands::Sealed`. The capability
/// created for it lands in whatever security context is currently active,
/// not necessarily the caller's `signing_key_id`'s owner.
/// Returns the new object's ObjID as a hex string.
#[pyfunction]
fn create_sealed_object(
    verifying_key_id: &str,
    signing_key_id: &str,
    message: &str,
) -> PyResult<String> {
    let spec = ObjectCreate::new(
        Default::default(),
        Default::default(),
        Some(parse_obj_id(verifying_key_id)?),
        Default::default(),
        Protections::empty(),
    );

    let base = MessageStoreObj::new(message).map_err(PyValueError::new_err)?;

    let s_key = Object::<SigningKey>::map(parse_obj_id(signing_key_id)?, MapFlags::READ)
        .map_err(to_py_err)?;

    let obj = ObjectBuilder::new(spec)
        .build_secure(base, s_key.base())
        .map_err(to_py_err)?;
    Ok(format!("{:x}", obj.id()))
}

/// Inspect an existing object, optionally activating a security context
/// first. Mirrors `ObjCommands::Inspect`. Returns a debug-formatted string.
#[pyfunction]
#[pyo3(signature = (obj_id, sec_ctx_id=None))]
fn inspect_object(obj_id: &str, sec_ctx_id: Option<&str>) -> PyResult<String> {
    if let Some(id) = sec_ctx_id {
        let sec_ctx = SecCtx::try_from(parse_obj_id(id)?).map_err(to_py_err)?;
        sec_ctx.set_active().map_err(to_py_err)?;
    }

    let target = Object::<MessageStoreObj>::map(parse_obj_id(obj_id)?, MapFlags::READ | MapFlags::WRITE)
        .map_err(to_py_err)?;

    Ok(format!("{:#?}", target.base()))
}

/// Create a new security context. Mirrors `CtxCommands::New`.
/// Returns the new security context's ObjID as a hex string.
#[pyfunction]
#[pyo3(signature = (undetachable=false, verifying_key_id=None))]
fn new_sec_ctx(undetachable: bool, verifying_key_id: Option<&str>) -> PyResult<String> {
    let flags = if undetachable {
        SecCtxFlags::UNDETACHABLE
    } else {
        SecCtxFlags::empty()
    };

    let mut ctx_builder = SecCtxBuilder::new()
        .default_protections(Protections::all())
        .global_mask(Protections::all())
        .flags(flags);

    if let Some(id) = verifying_key_id {
        ctx_builder = ctx_builder.verifying_key(parse_obj_id(id)?);
    }

    let sec_ctx = ctx_builder.build().map_err(to_py_err)?;
    Ok(format!("{:x}", sec_ctx.id()))
}

/// Inspect an existing security context. Mirrors `CtxCommands::Inspect`.
#[pyfunction]
fn inspect_sec_ctx(sec_ctx_id: &str) -> PyResult<String> {
    let sec_ctx = SecCtx::try_from(parse_obj_id(sec_ctx_id)?).map_err(to_py_err)?;
    Ok(format!("{sec_ctx:#?}"))
}

/// Create a capability for `target_obj` and insert it into `modifying_ctx`.
/// Mirrors `CtxAddCommands::Cap`.
#[pyfunction]
#[pyo3(signature = (signing_key_id, modifying_ctx, target_obj, protections, executing_ctx=None))]
fn add_capability(
    signing_key_id: &str,
    modifying_ctx: &str,
    target_obj: &str,
    protections: u16,
    executing_ctx: Option<&str>,
) -> PyResult<()> {
    maybe_activate(executing_ctx)?;

    let signing_key_id = parse_obj_id(signing_key_id)?;
    let modifying_ctx_id = parse_obj_id(modifying_ctx)?;
    let target_obj_id = parse_obj_id(target_obj)?;

    let s_key = Object::<SigningKey>::map(signing_key_id, MapFlags::READ).map_err(to_py_err)?;
    let mut modifying_sec_ctx = SecCtx::try_from(modifying_ctx_id).map_err(to_py_err)?;

    let cap = CapBuilder::new(target_obj_id, modifying_ctx_id)
        .protections(Protections::from_bits_truncate(protections))
        .build(s_key.base())
        .map_err(to_py_err)?;

    modifying_sec_ctx.insert_cap(cap).map_err(to_py_err)?;
    Ok(())
}

/// Delegate an existing capability/delegation for `target_obj` from
/// `provider_ctx` into `modifying_ctx`. Mirrors `CtxAddCommands::Del`.
#[pyfunction]
#[pyo3(signature = (signing_key_id, modifying_ctx, provider_ctx, target_obj, protections, executing_ctx=None))]
fn add_delegation(
    signing_key_id: &str,
    modifying_ctx: &str,
    provider_ctx: &str,
    target_obj: &str,
    protections: u16,
    executing_ctx: Option<&str>,
) -> PyResult<()> {
    maybe_activate(executing_ctx)?;

    let signing_key_id = parse_obj_id(signing_key_id)?;
    let modifying_ctx_id = parse_obj_id(modifying_ctx)?;
    let provider_ctx_id = parse_obj_id(provider_ctx)?;
    let target_obj_id = parse_obj_id(target_obj)?;

    let s_key = Object::<SigningKey>::map(signing_key_id, MapFlags::READ).map_err(to_py_err)?;
    let mut modifying_sec_ctx = SecCtx::try_from(modifying_ctx_id).map_err(to_py_err)?;
    let provider_sec_ctx = SecCtx::try_from(provider_ctx_id).map_err(to_py_err)?;

    let inner = *provider_sec_ctx
        .base()
        .map
        .get(&target_obj_id)
        .ok_or_else(|| PyRuntimeError::new_err("provider's security context has no entry for the target object"))?
        .last()
        .ok_or_else(|| PyRuntimeError::new_err("provider's map entry for the target object was empty"))?;

    let del = DelBuilder::new(provider_ctx_id, target_obj_id, inner)
        .receiver(modifying_sec_ctx.id())
        .prot_mask(Protections::from_bits_truncate(protections))
        .build(s_key.base())
        .map_err(to_py_err)?;

    modifying_sec_ctx.insert_del(del).map_err(to_py_err)?;
    Ok(())
}

#[pymodule]
pub fn twizzler_py(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(new_keypair, m)?)?;
    m.add_function(wrap_pyfunction!(create_object, m)?)?;
    m.add_function(wrap_pyfunction!(create_sealed_object, m)?)?;
    m.add_function(wrap_pyfunction!(inspect_object, m)?)?;
    m.add_function(wrap_pyfunction!(new_sec_ctx, m)?)?;
    m.add_function(wrap_pyfunction!(inspect_sec_ctx, m)?)?;
    m.add_function(wrap_pyfunction!(add_capability, m)?)?;
    m.add_function(wrap_pyfunction!(add_delegation, m)?)?;
    Ok(())
}
