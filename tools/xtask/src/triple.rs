use strum::IntoEnumIterator;
use strum_macros::EnumIter;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, EnumIter, clap::ArgEnum)]
pub enum Machine {
    Unknown,
    Rpi3,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, EnumIter, clap::ArgEnum)]
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

impl From<&Host> for String {
    fn from(h: &Host) -> Self {
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

impl From<&Arch> for String {
    fn from(a: &Arch) -> Self {
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

impl From<&Machine> for String {
    fn from(m: &Machine) -> Self {
        match m {
            Machine::Unknown => "unknown",
            Machine::Rpi3 => "rpi3",
        }
        .to_string()
    }
}

impl Machine {
    pub fn to_string(&self) -> String {
        self.into()
    }
}

impl Arch {
    pub fn to_string(&self) -> String {
        self.into()
    }
}

impl Host {
    pub fn to_string(&self) -> String {
        self.into()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Triple {
    machine: Machine,
    arch: Arch,
    host: Host,
}

impl From<&Triple> for String {
    fn from(t: &Triple) -> Self {
        format!(
            "{}-{}-{}",
            t.arch.to_string(),
            t.machine.to_string(),
            t.host.to_string()
        )
    }
}

impl Triple {
    pub fn new(arch: Arch, machine: Machine, host: Host) -> Self {
        Self {
            machine,
            arch,
            host,
        }
    }

    pub fn to_string(&self) -> String {
        self.into()
    }
}

#[allow(dead_code)]
pub fn all_possible_platforms(host: Host) -> Vec<Triple> {
    let mut triples = vec![];
    for arch in Arch::iter() {
        for machine in Machine::iter() {
            triples.push(Triple {
                machine,
                arch,
                host,
            })
        }
    }
    triples
}
