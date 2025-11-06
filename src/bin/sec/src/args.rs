use std::num::ParseIntError;

use clap::{Args, Parser, Subcommand, ValueEnum};
use twizzler::object::ObjID;
use twizzler_abi::{
    object::Protections,
    syscall::{
        ObjectCreate, ObjectCreateFlags, sys_sctx_attach, sys_thread_active_sctx_id,
        sys_thread_set_active_sctx_id,
    },
};

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
pub struct CliArgs {
    #[command(subcommand)]
    pub command: Commands,
}

// noun verb --args
#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Commands pertaining to security contexts
    #[command(subcommand)]
    Ctx(CtxCommands),
    /// Commands pertaining to singing/verifying keys
    #[command(subcommand)]
    Key(KeyCommands),

    /// Commands pertaining to objects.
    #[command(subcommand)]
    Obj(ObjCommands),

    Test,
    Create,
    Access(AccessArgs),
}

#[derive(Subcommand, Debug)]
pub enum CtxCommands {
    New(NewCtxArgs),

    Inspect(CtxInspectArgs),
}

#[derive(Subcommand, Debug)]
pub enum KeyCommands {
    #[command(short_flag = 'n')]
    NewPair,
}

#[derive(Subcommand, Debug)]
pub enum ObjCommands {
    /// Create a new object.
    New(NewObjectArgs),

    /// Inspect an existing object.
    Inspect(ObjInspectArgs),
}

#[derive(Args, Debug)]
pub struct NewObjectArgs {
    /// the verifyign key to use when creating the object
    #[arg(short = 'v', long, value_parser=parse_obj_id)]
    pub verifying_key_id: ObjID,

    /// After creating this object, it will have no default permissions
    #[arg(short, long, default_value = "false")]
    pub seal: bool,

    /// simple string message to store inside the object
    #[arg(short, long)]
    pub message: String,
}

#[derive(Args, Debug)]
pub struct ObjInspectArgs {
    /// the security context to use when inspecting this object
    #[arg(short = 's', long, value_parser=parse_obj_id)]
    pub sec_ctx_id: Option<ObjID>,

    /// the object to be inspected
    #[arg(short = 'o', long, value_parser=parse_obj_id)]
    pub obj_id: ObjID,
}

#[derive(Args, Debug)]
pub struct CtxInspectArgs {
    /// the security context to be inspected
    #[arg(short = 's', long, value_parser=parse_obj_id)]
    pub sec_ctx_id: ObjID,
}

fn parse_obj_id(arg: &str) -> Result<ObjID, ParseIntError> {
    let as_num = u128::from_str_radix(arg, 16)?;
    Ok(ObjID::from(as_num))
}

#[derive(Args, Debug)]
pub struct NewCtxArgs {
    #[arg(short, long, default_value = "false")]
    pub undetachable: bool,
}

#[derive(Debug, Args)]
pub struct AccessArgs {
    #[arg(short, long)]
    pub obj_id: String,
    #[arg(short, long)]
    pub sec_ctx_id: String,
}
