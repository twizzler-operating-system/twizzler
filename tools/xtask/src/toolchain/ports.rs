use std::path::Path;

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
    pub ports: Vec<String>,
}

pub fn list_ports() -> anyhow::Result<()> {
    let ports = vec![
        ("python3", "zlib,openssl,ncurses"),
        ("llvm", "zlib"),
        ("zlib", ""),
        ("ncurses", ""),
        //("rust", ""),
        ("openssl", "zlib"),
        ("curl", "zlib,openssl"),
        ("libssh2", "zlib,openssl"),
        ("libgit2", "zlib,openssl,libssh2"),
        ("neatvi", ""),
        ("psl", ""),
        ("binutils", ""),
    ];

    for port in ports {
        if port.1.is_empty() {
            println!("{}", port.0);
        } else {
            println!("{} (requires {})", port.0, port.1);
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

    for port in &cli.ports {
        if port == "@all" {
            build_ports(&triple)?;
            continue;
        }
        match port.as_str() {
            "python3" => python3::install(&triple)?,
            "llvm" => llvm::install(&triple)?,
            "zlib" => zlib::install(&triple)?,
            "ncurses" => ncurses::install(&triple)?,
            // in-progress support
            "rust" => rust::install(&triple)?,
            "openssl" => openssl::install(&triple)?,
            "curl" => curl::install(&triple)?,
            "libssh2" => libssh2::install(&triple)?,
            "libgit2" => libgit2::install(&triple)?,
            "neatvi" => neatvi::install(&triple)?,
            "psl" => psl::install(&triple)?,
            "binutils" => binutils::install(&triple)?,
            _ => anyhow::bail!("Unknown port: {}", port),
        }
    }

    Ok(())
}

fn build_ports(triple: &Triple) -> anyhow::Result<()> {
    python3::install(triple)?;
    zlib::install(triple)?;
    ncurses::install(triple)?;
    llvm::install(triple)?;
    //rust::install(triple)?;
    openssl::install(triple)?;
    psl::install(triple)?;
    curl::install(triple)?;
    libssh2::install(triple)?;
    libgit2::install(triple)?;
    neatvi::install(triple)?;
    binutils::install(triple)?;

    Ok(())
}
