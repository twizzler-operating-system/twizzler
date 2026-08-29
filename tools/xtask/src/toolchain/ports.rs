use std::{collections::HashSet, path::Path};

use crate::triple::{Arch, Host, Machine, Triple};

mod binutils;
mod curl;
mod libgit2;
mod libssh2;
mod llvm;
mod ncurses;
mod neatvi;
mod openssl;
mod psl;
mod python3;
mod rust;
mod zlib;

/// A package whose bundled libtool.m4 predates the twizzler triple silently disables shared
/// libraries: `build_libtool_libs=no`, empty `archive_cmds`/`library_names_spec`, and the
/// unknown-OS defaults. Rewrite the generated `libtool` script with the standard GNU-ld ELF
/// behavior, and bake `-target`/`--sysroot` into `CC` — libtool strips flags it doesn't
/// recognize from link lines, so leaving them in CFLAGS/LDFLAGS links against the host.
pub(crate) fn patch_libtool_for_twizzler(
    libtool: &Path,
    cc: &str,
    triple: &Triple,
    sysroot: &Path,
) -> anyhow::Result<()> {
    let mut s = std::fs::read_to_string(libtool)?;

    let cc_line = format!("\nCC=\"{}\"\n", cc);
    let cc_patched = format!(
        "\nCC=\"{} -target {} --sysroot {}\"\n",
        cc,
        triple,
        sysroot.display()
    );
    let pairs: &[(&str, &str)] = &[
        ("\nbuild_libtool_libs=no\n", "\nbuild_libtool_libs=yes\n"),
        (
            r#"deplibs_check_method="unknown""#,
            r#"deplibs_check_method="pass_all""#,
        ),
        ("\nversion_type=none\n", "\nversion_type=linux\n"),
        ("\nneed_lib_prefix=unknown\n", "\nneed_lib_prefix=no\n"),
        ("\nneed_version=unknown\n", "\nneed_version=no\n"),
        (
            r#"library_names_spec="""#,
            r#"library_names_spec="\$libname\$release\$shared_ext\$versuffix \$libname\$release\$shared_ext\$major \$libname\$shared_ext""#,
        ),
        (
            r#"soname_spec="""#,
            r#"soname_spec="\$libname\$release\$shared_ext\$major""#,
        ),
        ("\nshlibpath_var=\n", "\nshlibpath_var=LD_LIBRARY_PATH\n"),
        (
            r#"archive_cmds="""#,
            r#"archive_cmds="\$CC -shared \$pic_flag \$libobjs \$deplibs \$compiler_flags \$wl-soname \$wl\$soname -o \$lib""#,
        ),
        (
            r#"archive_expsym_cmds="""#,
            r#"archive_expsym_cmds="echo '{ global:' > \$output_objdir/\$libname.ver~cat \$export_symbols | \$SED -e 's/\(.*\)/\1;/' >> \$output_objdir/\$libname.ver~echo 'local: *; };' >> \$output_objdir/\$libname.ver~\$CC -shared \$pic_flag \$libobjs \$deplibs \$compiler_flags \$wl-soname \$wl\$soname \$wl-version-script \$wl\$output_objdir/\$libname.ver -o \$lib""#,
        ),
    ];

    // Patch the first (C-tag) occurrence only; later tag sections (CXX) stay untouched.
    for (old, new) in pairs {
        s = s.replacen(old, new, 1);
    }
    s = s.replacen(&cc_line, &cc_patched, 1);

    if !s.contains("build_libtool_libs=yes") {
        anyhow::bail!(
            "libtool patch failed: {} does not look like an unknown-OS libtool script",
            libtool.display()
        );
    }
    std::fs::write(libtool, s)?;
    Ok(())
}

#[derive(clap::Parser, Debug)]
pub struct PortOptions {
    #[clap(long, help = "The target architecture.", default_value = "x86-64")]
    pub arch: Arch,
    #[clap(
        long,
        help = "Build only the named ports, without first building what they depend on."
    )]
    pub no_deps: bool,
    pub ports: Vec<String>,
}

/// Known ports: name, the ports it must be built after, and whether `@all` includes it.
const PORTS: &[(&str, &[&str], bool)] = &[
    ("zlib", &[], true),
    ("ncurses", &[], true),
    ("psl", &[], true),
    ("neatvi", &[], true),
    ("binutils", &[], true),
    ("openssl", &["zlib"], true),
    ("llvm", &["zlib"], true),
    ("libssh2", &["zlib", "openssl"], true),
    ("curl", &["zlib", "openssl", "libssh2"], true),
    ("libgit2", &["zlib", "openssl", "libssh2"], true),
    ("python3", &["zlib", "openssl", "ncurses"], true),
    // In-progress support: buildable by name, but not part of @all.
    ("rust", &[], false),
];

fn port_deps(name: &str) -> anyhow::Result<&'static [&'static str]> {
    PORTS
        .iter()
        .find(|(port, _, _)| *port == name)
        .map(|(_, deps, _)| *deps)
        .ok_or_else(|| anyhow::anyhow!("Unknown port: {}", name))
}

pub fn list_ports() -> anyhow::Result<()> {
    for (name, deps, in_all) in PORTS {
        if !in_all {
            continue;
        }
        if deps.is_empty() {
            println!("{}", name);
        } else {
            println!("{} (requires {})", name, deps.join(","));
        }
    }

    println!("\nTo compile all ports, run cargo toolchain ports @all");

    Ok(())
}

pub fn build_and_install_ports(cli: &PortOptions) -> anyhow::Result<()> {
    let triple = Triple::new(cli.arch, Machine::Unknown, Host::Twizzler, None);
    if cli.ports.is_empty() {
        return list_ports();
    }

    let mut requested = Vec::new();
    for port in &cli.ports {
        if port == "@all" {
            requested.extend(
                PORTS
                    .iter()
                    .filter(|(_, _, in_all)| *in_all)
                    .map(|(name, _, _)| *name),
            );
        } else {
            // Reject unknown names before building anything.
            let (name, _, _) = PORTS
                .iter()
                .find(|(name, _, _)| name == port)
                .ok_or_else(|| anyhow::anyhow!("Unknown port: {}", port))?;
            requested.push(*name);
        }
    }

    let mut attempted = HashSet::new();
    let mut failed = Vec::new();
    for port in requested {
        build_port(port, &triple, cli.no_deps, &mut attempted, &mut failed)?;
    }

    if !failed.is_empty() {
        anyhow::bail!("failed to build ports: {}", failed.join(", "));
    }

    Ok(())
}

/// Build `name` after its dependencies, or alone under `no_deps`. A port is attempted at most once
/// per run, whether it succeeded or failed, so a shared dependency is not rebuilt and a failure
/// does not stall the remaining ports.
fn build_port(
    name: &'static str,
    triple: &Triple,
    no_deps: bool,
    attempted: &mut HashSet<&'static str>,
    failed: &mut Vec<&'static str>,
) -> anyhow::Result<()> {
    if !attempted.insert(name) {
        return Ok(());
    }

    if !no_deps {
        for dep in port_deps(name)? {
            build_port(dep, triple, no_deps, attempted, failed)?;
        }
    }

    println!("=== building port {}", name);
    if let Err(e) = install_port(name, triple) {
        eprintln!("=== port {} failed: {:?}", name, e);
        failed.push(name);
    }

    Ok(())
}

fn install_port(name: &str, triple: &Triple) -> anyhow::Result<()> {
    match name {
        "python3" => python3::install(triple),
        "llvm" => llvm::install(triple),
        "zlib" => zlib::install(triple),
        "ncurses" => ncurses::install(triple),
        "rust" => rust::install(triple),
        "openssl" => openssl::install(triple),
        "curl" => curl::install(triple),
        "libssh2" => libssh2::install(triple),
        "libgit2" => libgit2::install(triple),
        "neatvi" => neatvi::install(triple),
        "psl" => psl::install(triple),
        "binutils" => binutils::install(triple),
        _ => anyhow::bail!("Unknown port: {}", name),
    }
}
