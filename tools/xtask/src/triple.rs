use std::fmt::Display;

use strum_macros::EnumIter;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, EnumIter, clap::ValueEnum)]
pub enum Machine {
    Unknown,
    Rpi3,
    Virt,
    Morello,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, EnumIter, clap::ValueEnum)]
pub enum Arch {
    X86_64,
    Aarch64,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Host {
    None,
    Twizzler,
    Build,
}

fn get_build_host_name() -> &'static str {
    std::env::consts::OS
}

impl From<Host> for String {
    fn from(h: Host) -> Self {
        match h {
            Host::None => "none",
            Host::Twizzler => "twizzler",
            Host::Build => get_build_host_name(),
        }
        .to_string()
    }
}

impl TryFrom<&str> for Arch {
    type Error = ();

    fn try_from(s: &str) -> Result<Self, ()> {
        Ok(match s {
            "aarch64" => Self::Aarch64,
            "x86_64" => Self::X86_64,
            _ => return Err(()),
        })
    }
}

impl From<Arch> for String {
    fn from(a: Arch) -> Self {
        match a {
            Arch::X86_64 => "x86_64",
            Arch::Aarch64 => "aarch64",
        }
        .to_string()
    }
}

impl TryFrom<&str> for Machine {
    type Error = ();

    fn try_from(s: &str) -> Result<Self, ()> {
        Ok(match s {
            "unknown" => Self::Unknown,
            "rpi3" => Self::Rpi3,
            _ => return Err(()),
        })
    }
}

impl From<Machine> for String {
    fn from(m: Machine) -> Self {
        match m {
            Machine::Unknown => "unknown",
            Machine::Rpi3 => "rpi3",
            Machine::Virt => "virt",
            Machine::Morello => "morello",
        }
        .to_string()
    }
}

impl Display for Machine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", Into::<String>::into(*self))
    }
}

impl Display for Arch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", Into::<String>::into(*self))
    }
}

impl Display for Host {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", Into::<String>::into(*self))
    }
}
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Triple {
    pub machine: Machine,
    pub arch: Arch,
    pub host: Host,
    env: Option<String>,
}

impl From<Triple> for String {
    fn from(t: Triple) -> Self {
        if let Some(e) = t.env.as_ref() {
            format!("{}-{}-{}-{}", t.arch, t.machine, t.host, e)
        } else {
            format!("{}-{}-{}", t.arch, t.machine, t.host)
        }
    }
}

impl Triple {
    pub fn new(arch: Arch, machine: Machine, host: Host, env: Option<&str>) -> Self {
        Self {
            machine,
            arch,
            host,
            env: env.map(|s| s.to_string()),
        }
    }
}

impl Display for Triple {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", Into::<String>::into(self.clone()))
    }
}

pub fn all_possible_platforms() -> Vec<Triple> {
    let triples = vec![
        Triple::new(Arch::Aarch64, Machine::Unknown, Host::Twizzler, None),
        Triple::new(Arch::X86_64, Machine::Unknown, Host::Twizzler, None),
        /*
        Triple::new(
            Arch::Aarch64,
            Machine::Unknown,
            Host::Twizzler,
            Some("minruntime"),
        ),
        Triple::new(
            Arch::X86_64,
            Machine::Unknown,
            Host::Twizzler,
            Some("minruntime"),
        ),
        */
    ];
    triples
}

pub fn valid_targets() -> Vec<(Arch, Machine)> {
    let targets = vec![
        (Arch::X86_64, Machine::Unknown),
        (Arch::Aarch64, Machine::Virt),
        (Arch::Aarch64, Machine::Morello),
    ];
    targets
}
