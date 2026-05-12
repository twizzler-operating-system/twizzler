use std::{io::Write, path::Path, process::Command, thread::available_parallelism};

use reqwest::Client;

use crate::{toolchain::download_file, triple::Triple};

const GAS_CONFIG_PATCH: &str = r#"
130a131,132
>   aarch64*-*-twizzler*)                       fmt=elf;;
>   x86_64*-*-twizzler*)                        fmt=elf;;
"#;

const BFDCONFIG_PATCH: &str = r#"
704a705,714
>   x86_64-*-twizzler*)
>     targ_defvec=x86_64_elf64_vec
>     targ_selvecs="i386_elf32_vec"
>     want64=true
>     ;;
>   aarch64-*-twizzler*)
>     targ_defvec=aarch64_elf64_vec
>     targ_selvecs=""
>     want64=true
>     ;;
"#;

pub fn install(triple: &Triple) -> anyhow::Result<()> {
    println!("Building binutils for {}", triple);

    let sysroot_dir = Path::new("toolchain/install/sysroots")
        .join(triple.to_string())
        .canonicalize()?;

    let url = "https://sourceware.org/pub/binutils/releases/binutils-2.46.0.tar.xz";

    let cont_dir = Path::new("toolchain/install/build/ports/binutils");
    std::fs::create_dir_all(&cont_dir)?;
    let cont_dir = cont_dir.canonicalize()?;
    let tar_file = cont_dir.join("binutils-2.46.0.tar.xz");
    if !std::fs::exists(&tar_file)? {
        let client = Client::new();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(download_file(
                &client,
                url,
                tar_file.display().to_string().as_str(),
            ))?;
    }

    let status = std::process::Command::new("tar")
        .arg("-xJf")
        .arg("binutils-2.46.0.tar.xz")
        .current_dir("toolchain/install/build/ports/binutils")
        .status()?;

    if !status.success() {
        anyhow::bail!("failed to extract binutils");
    }

    let src_dir =
        Path::new("toolchain/install/build/ports/binutils/binutils-2.46.0").canonicalize()?;
    let build_dir =
        Path::new("toolchain/install/build/ports/binutils/build").join(triple.to_string());
    let install_dir = Path::new("toolchain/install/sysroots").join(&triple.to_string());
    std::fs::create_dir_all(&install_dir)?;
    std::fs::create_dir_all(&build_dir)?;
    let build_dir = build_dir.canonicalize()?;
    let _ = std::fs::remove_dir_all(&build_dir);
    std::fs::create_dir_all(&build_dir)?;
    let install_dir = install_dir.canonicalize()?;

    let bin_dir = Path::new("toolchain/install/bin").canonicalize()?;

    let mut cmd = Command::new("patch")
        .arg(src_dir.join("bfd/config.bfd").display().to_string())
        .current_dir(&src_dir)
        .stdin(std::process::Stdio::piped())
        .spawn()?;

    {
        let mut stdin = cmd.stdin.as_ref().unwrap();
        stdin.write_all(BFDCONFIG_PATCH.as_bytes())?;
    }

    cmd.wait()?;

    let mut cmd = Command::new("patch")
        .arg(src_dir.join("gas/configure.tgt").display().to_string())
        .current_dir(&src_dir)
        .stdin(std::process::Stdio::piped())
        .spawn()?;

    {
        let mut stdin = cmd.stdin.as_ref().unwrap();
        stdin.write_all(GAS_CONFIG_PATCH.as_bytes())?;
    }

    cmd.wait()?;

    let mut cmd = Command::new(src_dir.join("configure"));
    cmd.current_dir(&build_dir);

    cmd.arg("--host")
        .arg(triple.to_string())
        .arg("--target")
        .arg(triple.to_string())
        .arg("--build")
        .arg(crate::toolchain::guess_host_triple().unwrap())
        .arg("--prefix=/pkg/binutils")
        .arg("--enable-shared")
        .arg("--enable-ld=no")
        .arg("--enable-gprof=no")
        .arg("--enable-gas=no")
        .arg("--with-system-zlib")
        .arg("ac_cv_have_malloc_h=no")
        .arg("ac_cv_have_decl_basename=yes")
        .arg("ac_cv_header_stdc=yes")
        .arg("--enable-optimizations");
    cmd.env("DESTDIR", &install_dir);

    let cflags = format!(
        "-target {} --sysroot {} -fPIC -DHAVE_DECL_BASENAME=1 -O2",
        triple,
        sysroot_dir.display()
    );

    let set_env = |cmd: &mut Command| {
        cmd.env("ac_cv_header_stdc", "yes");
        cmd.env("PKG_CONFIG", "");
        cmd.env("CFLAGS", &cflags);
        cmd.env("LIBS", "-lz");
        cmd.env("CXXFLAGS", &cflags);
        cmd.env("CPPFLAGS", &cflags);
        cmd.arg(format!("CPPFLAGS={}", &cflags));
        cmd.env("LDFLAGS", cflags.clone() + " -lz");
        cmd.env("CC", bin_dir.join("clang").display().to_string());
        cmd.env(
            "CPP",
            bin_dir.join("clang").display().to_string() + " -E " + &cflags,
        );
        cmd.arg(
            "CPP=".to_string() + &bin_dir.join("clang").display().to_string() + " -E " + &cflags,
        );
        cmd.env("CXX", bin_dir.join("clang++").display().to_string());
        cmd.env("LD", bin_dir.join("clang").display().to_string());
        let mut lds = bin_dir.join("clang").display().to_string();
        lds.push_str(" -shared ");
        lds.push_str(&cflags);
        cmd.env("LDSHARED", lds);
        cmd.env("AR", bin_dir.join("llvm-ar").display().to_string());
        cmd.env("RANLIB", bin_dir.join("llvm-ranlib").display().to_string());
    };

    set_env(&mut cmd);

    let mut ch = cmd.spawn()?;
    if !ch.wait()?.success() {
        anyhow::bail!("failed to configure binutils");
    }

    let mut cmd = Command::new("make");
    cmd.current_dir(&build_dir)
        .arg("-j")
        .arg(available_parallelism().unwrap().get().to_string());
    set_env(&mut cmd);

    let status = cmd.status()?;
    if !status.success() {
        anyhow::bail!("failed to build binutils");
    }

    let mut cmd = Command::new("make");
    cmd.current_dir(&build_dir)
        .arg("install")
        .arg(format!("DESTDIR={}", sysroot_dir.display()))
        .arg("-j")
        .arg(available_parallelism().unwrap().get().to_string());
    set_env(&mut cmd);

    let status = cmd.status()?;
    if !status.success() {
        anyhow::bail!("failed to install binutils");
    }

    Ok(())
}
