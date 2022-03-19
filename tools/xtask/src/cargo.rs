use std::{
    io,
    path::Path,
    process::{Command, ExitStatus, Output},
};

use crate::{toolchain::Toolchain, triple::Triple};

pub struct CargoCommand {
    toolchain: Toolchain,
    target: Triple,
    crates: Vec<Crate>,
    outdir: Box<Path>,
    command: String,
    arguments: Vec<String>,
    rustc_args: Vec<String>,
}

pub enum CrateType {
    Lib,
    Bin,
}

impl CrateType {
    pub fn to_string(&self) -> String {
        match self {
            CrateType::Lib => "lib",
            CrateType::Bin => "bin",
        }
        .to_string()
    }
}

pub struct Crate {
    name: String,
    ty: CrateType,
}

impl CargoCommand {
    pub fn new(command: String, toolchain: Toolchain, target: Triple, outdir: Box<Path>) -> Self {
        Self {
            toolchain,
            target,
            crates: vec![],
            outdir,
            command,
            arguments: vec![],
            rustc_args: vec![],
        }
    }

    pub fn rustc_arg(&mut self, arg: String) -> &mut Self {
        self.rustc_args.push(arg);
        self
    }

    pub fn arg(&mut self, arg: String) -> &mut Self {
        self.arguments.push(arg);
        self
    }

    pub fn add_crate(&mut self, cr: Crate) -> &mut Self {
        self.crates.push(cr);
        self
    }

    fn build_cmd(&self) -> Command {
        let mut command = Command::new("cargo");
        command
            .arg(&self.command)
            .arg("--target")
            .arg(self.target.to_string())
            .arg("--target-dir")
            .arg(self.outdir.to_path_buf())
            .args(&self.arguments)
            .env("RUSTC_FLAGS", self.rustc_args.join(" "))
            .env("RUSTUP_TOOLCHAIN", self.toolchain.name());

        for cr in &self.crates {
            command
                .arg(format!("--{}", cr.ty.to_string()))
                .arg(&cr.name);
        }

        command
    }

    pub fn execute(&self) -> io::Result<ExitStatus> {
        let mut command = self.build_cmd();
        command.status()
    }

    pub fn execute_capture(&self) -> io::Result<Output> {
        let mut command = self.build_cmd();
        command.output()
    }
}
