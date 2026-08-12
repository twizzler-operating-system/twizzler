use std::{
    fs::File,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::Context;
use cargo::core::compiler::{Compilation, CompileTarget};

use crate::{
    build::TwizzlerCompilation,
    disk::copy_twizzler_build,
    toolchain::{get_sysroots_path, get_toolchain_path},
    triple::Arch,
    BuildConfig, ImageOptions,
};

pub struct ImageInfo {
    pub disk_image: PathBuf,
}

fn do_get_crate_initrd_files(comp: &Compilation, crate_name: &str) -> anyhow::Result<Vec<PathBuf>> {
    let unit = comp
        .binaries
        .iter()
        .find(|item| item.unit.pkg.name() == crate_name)
        .with_context(|| format!("failed to find initrd crate {}", crate_name))?;

    Ok(vec![unit.path.clone()])
}

fn get_crate_initrd_files(
    comp: &TwizzlerCompilation,
    crate_name: &str,
) -> anyhow::Result<Vec<PathBuf>> {
    if let Ok(r) = do_get_crate_initrd_files(
        comp.borrow_user_compilation()
            .as_ref()
            .expect("userspace not compiled"),
        crate_name,
    ) {
        return Ok(r);
    }
    do_get_crate_initrd_files(
        comp.borrow_static_compilation()
            .as_ref()
            .expect("userspace-static not compiled"),
        crate_name,
    )
}

fn get_third_party_initrd_files(
    comp: &TwizzlerCompilation,
    crate_name: &str,
) -> anyhow::Result<Vec<PathBuf>> {
    comp.borrow_third_party_compilation()
        .iter()
        .map(|c| {
            c.binaries
                .iter()
                .find(|item| item.unit.pkg.name() == crate_name)
                .with_context(|| format!("failed to find initrd third-party crate {}", crate_name))
                .map(|x| x.path.clone())
        })
        .collect()
}

fn get_lib_initrd_files(
    comp: &TwizzlerCompilation,
    name: &str,
    config: &BuildConfig,
) -> anyhow::Result<Vec<PathBuf>> {
    let comp = comp.borrow_user_compilation().as_ref().unwrap();
    let mut output = comp
        .root_output
        .get(&cargo::core::compiler::CompileKind::Target(
            CompileTarget::new(&config.twz_triple().to_string())?,
        ))
        .cloned()
        .unwrap();

    output.push(format!("lib{}.so", name).replace("-", "_"));
    if output.is_file() {
        Ok(vec![output])
    } else {
        anyhow::bail!(
            "library file {} ({}) is a not a file or does not exist",
            name,
            output.display()
        );
    }
}

fn get_tool_path<'a>(comp: &'a TwizzlerCompilation, name: &str) -> anyhow::Result<&'a Path> {
    let unit = comp
        .borrow_tools_compilation()
        .binaries
        .iter()
        .find(|item| item.unit.pkg.name() == name)
        .with_context(|| format!("failed to find initrd crate {}", name))?;
    Ok(&unit.path)
}

fn get_genfile_path(comp: &TwizzlerCompilation, name: &str) -> PathBuf {
    let mut path = comp.get_kernel_image(false).parent().unwrap().to_path_buf();
    path.push(name);
    path
}

/// Everything in here is packed into the initrd verbatim, and the initrd is read through UEFI block
/// I/O at boot (~90MB/s), so a stray file here is paid for on every single boot. `src/data` is
/// gitignored scratch, which makes that easy to miss -- hence the size warning.
const DATA_FOLDER_WARN_BYTES: u64 = 8 << 20;

fn generate_data_folder(comp: &TwizzlerCompilation, data_override: Option<&Path>) -> PathBuf {
    let mut destination = comp.get_kernel_image(false).parent().unwrap().to_path_buf();
    destination.push("data/");

    let source = data_override
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("./src/data/"));
    if source.exists() {
        let _ = Command::new("rsync")
            .arg("-a")
            .arg("--delete")
            .arg(format!("{}/", source.display()))
            .arg(&destination)
            .status();
    } else {
        // No source means no data files. Without this the last rsync's output survives in the
        // build tree and keeps getting packed, with nothing left to explain where it came from.
        let _ = std::fs::remove_dir_all(&destination);
    }

    let total: u64 = walkdir::WalkDir::new(&destination)
        .min_depth(1)
        .into_iter()
        .flatten()
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum();
    if total >= DATA_FOLDER_WARN_BYTES {
        eprintln!(
            "warning: {} adds {:.0}MB to the initrd, costing roughly {:.1}s of boot time per run",
            source.display(),
            total as f64 / (1 << 20) as f64,
            total as f64 / (90.0 * (1 << 20) as f64),
        );
    }

    destination
}

fn is_elf(path: &Path) -> bool {
    let mut magic = [0u8; 4];
    File::open(path)
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut magic))
        .is_ok_and(|()| magic == *b"\x7fELF")
}

/// Copy every initrd file into a staging directory with its DWARF removed, and return the staged
/// paths.
///
/// The initrd is read whole through UEFI block I/O at boot, and it is almost entirely debug info --
/// 229MB of payload drops to 38MB. Nothing in the guest needs that DWARF: the host symbolizes
/// against the unstripped originals in `target/`, which is also what the debug sysroot extracted
/// below is for.
///
/// `--strip-debug`, not `--strip-all`: it keeps `.symtab` and `.dynsym`, so dynamic linking and
/// symbol-level in-guest backtraces both still work. Non-ELF entries (the generated `test_bins`
/// list and friends are plain text) are staged unchanged -- checked by magic rather than by letting
/// `llvm-strip` fail, which would spray "not a valid object file" through the build output.
fn strip_initrd_files(
    initrd_files: &[PathBuf],
    comp: &TwizzlerCompilation,
) -> anyhow::Result<Vec<PathBuf>> {
    let stage = get_genfile_path(comp, "initrd-stage");
    let _ = std::fs::remove_dir_all(&stage);
    std::fs::create_dir_all(&stage)?;

    let strip = get_toolchain_path()?.join("bin/llvm-strip");
    let mut staged = Vec::with_capacity(initrd_files.len());
    for src in initrd_files {
        let dst = stage.join(
            src.file_name()
                .with_context(|| format!("initrd entry {} has no file name", src.display()))?,
        );
        let stripped = is_elf(src)
            && Command::new(&strip)
                .arg("--strip-debug")
                .arg("-o")
                .arg(&dst)
                .arg(src)
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
        if !stripped {
            std::fs::copy(src, &dst)
                .with_context(|| format!("failed to stage initrd entry {}", src.display()))?;
        }
        staged.push(dst);
    }
    Ok(staged)
}

fn generate_initrd(
    initrd_files: Vec<PathBuf>,
    data_files: PathBuf,
    comp: &TwizzlerCompilation,
) -> anyhow::Result<PathBuf> {
    let initrd_path = get_genfile_path(comp, "initrd");
    let status = Command::new(get_tool_path(comp, "initrd_gen")?)
        .arg("--data")
        .arg(&data_files)
        .arg("--output")
        .arg(&initrd_path)
        .args(&initrd_files)
        .status()?;

    if status.success() {
        Ok(initrd_path)
    } else {
        anyhow::bail!("failed to generate initrd");
    }
}

fn build_initrd(cli: &ImageOptions, comp: &TwizzlerCompilation) -> anyhow::Result<Vec<PathBuf>> {
    crate::print_status_line("initrd", Some(&cli.config));
    // Create an empty initrd if we are building just the kernel.
    // No user space components are required to run the code, but
    // the bootloader (limine) is configured to expect initrd,
    // even if we are not going to use it.
    if cli.kernel {
        let initrd_path = get_genfile_path(comp, "initrd");
        let result = File::create(&initrd_path);
        if result.is_ok() {
            Ok(vec![initrd_path])
        } else {
            anyhow::bail!("failed to generate initrd");
        }
    } else {
        let initrd_meta = comp
            .borrow_user_workspace()
            .custom_metadata()
            .expect("no initrd specification in Cargo.toml")
            .get("initrd")
            .expect("no initrd specification in Cargo.toml");

        let mut initrd_files = vec![];
        for item in initrd_meta
            .as_array()
            .expect("initrd specification must be an array")
        {
            let spec = item.as_str().expect("initrd item must be a string");
            let split: Vec<_> = spec.split(':').into_iter().collect();
            if split.len() != 2 {
                anyhow::bail!("initrd item must be of the form `x:y'");
            }
            match split[0] {
                "lib" => {
                    initrd_files.append(&mut get_lib_initrd_files(comp, split[1], &cli.config)?)
                }
                "crate" => initrd_files.append(&mut get_crate_initrd_files(comp, split[1])?),
                "third-party" => {
                    initrd_files.append(&mut get_third_party_initrd_files(comp, split[1])?)
                }
                x => panic!("invalid initrd spec {}", x),
            }
        }

        // all the tests for init to run. Only the *list* goes in the initrd --
        // `copy_twizzler_build` stages the binaries themselves on the disk, where they are
        // demand-paged instead of being read whole at boot. `unittest` resolves each name
        // against the disk and falls back to the initrd, so an older image still works.
        if let Some(ref test_comp) = comp.borrow_user_test_compilation() {
            let mut testlist = String::new();
            for bin in test_comp.tests.iter() {
                testlist += &bin.path.file_name().unwrap().to_string_lossy();
                testlist += "\n";
            }
            if cli.tests {
                let test_file_path = get_genfile_path(comp, "test_bins");
                let mut file = File::create(&test_file_path)?;
                file.write_all(testlist.as_bytes())?;
                initrd_files.push(test_file_path);
            }
            if cli.benches {
                let test_file_path = get_genfile_path(comp, "bench_bins");
                let mut file = File::create(&test_file_path)?;
                file.write_all(testlist.as_bytes())?;
                initrd_files.push(test_file_path);
            }
            if let Some(bench) = &cli.bench {
                let test_file_path = get_genfile_path(comp, "bench_bin");
                let mut file = File::create(&test_file_path)?;
                file.write_all(bench.as_bytes())?;
                initrd_files.push(test_file_path);
            }
        } else {
            assert!(!cli.tests && !cli.benches && !cli.bench.is_some());
        }

        // Standalone (plain main(), not #[test]) programs opted into `unittest` via
        // `[workspace.metadata] test-programs` in the root Cargo.toml. Unlike the #[test]
        // binaries above, these are ordinary Userspace-collection binaries, so they are not
        // auto-added to the initrd by virtue of being compiled -- push each one in explicitly, or
        // `unittest` will try to spawn a name that isn't there and every entry reports
        // SpawnFailed.
        if cli.tests {
            if let Some(test_programs) = comp
                .borrow_user_workspace()
                .custom_metadata()
                .and_then(|v| v.get("test-programs"))
            {
                let mut testlist = String::new();
                for item in test_programs
                    .as_array()
                    .expect("test-programs specification must be an array")
                {
                    let crate_name = item
                        .get("crate")
                        .and_then(|v| v.as_str())
                        .expect("test-programs entry missing string `crate` key");
                    let args = item
                        .get("args")
                        .map(|v| {
                            v.as_array()
                                .expect("test-programs `args` must be an array")
                                .iter()
                                .map(|s| {
                                    s.as_str()
                                        .expect("test-programs `args` entries must be strings")
                                })
                        })
                        .into_iter()
                        .flatten();

                    for path in get_crate_initrd_files(comp, crate_name)? {
                        let already_present = initrd_files
                            .iter()
                            .any(|p| p.file_name() == path.file_name());
                        if !already_present {
                            initrd_files.push(path);
                        }
                    }

                    testlist += crate_name;
                    for arg in args {
                        testlist += " ";
                        testlist += arg;
                    }
                    testlist += "\n";
                }
                let test_file_path = get_genfile_path(comp, "standalone_test_bins");
                let mut file = File::create(&test_file_path)?;
                file.write_all(testlist.as_bytes())?;
                initrd_files.push(test_file_path);
            }
        }

        let montest = comp
            .borrow_user_test_compilation()
            .as_ref()
            .and_then(|utc| {
                utc.tests
                    .iter()
                    .find_map(|p| {
                        if p.unit.pkg.name() == "montest" {
                            Some(p.path.file_name().unwrap().to_string_lossy().to_owned())
                        } else {
                            None
                        }
                    })
                    .clone()
            });

        if let Some(montest) = montest {
            let test_file_path = get_genfile_path(comp, "monitor_test_info");
            let mut file = File::create(&test_file_path)?;
            file.write_all(montest.as_bytes())?;
            initrd_files.push(test_file_path);
        }

        let mut lib_path = crate::toolchain::get_rustlib_lib(&cli.config.twz_triple().to_string())?;
        let mut testso_path = crate::toolchain::get_rust_stage2_std(
            guess_host_triple::guess_host_triple().unwrap(),
            &cli.config.twz_triple().to_string(),
        )?;
        testso_path.push("libtest.so");
        let libstd_name = walkdir::WalkDir::new(lib_path.clone())
            .min_depth(1)
            .max_depth(2)
            .into_iter()
            .filter_entry(|x| {
                x.file_name()
                    .to_str()
                    .map(|s| s.starts_with("libstd-") && s.ends_with(".so"))
                    .unwrap_or(false)
            })
            .next()
            .ok_or(anyhow::anyhow!(
                "failed to find libstd in {}",
                lib_path.display()
            ))??
            .file_name()
            .to_str()
            .unwrap()
            .to_owned();

        lib_path.push(libstd_name);
        initrd_files.push(lib_path);

        let lib_path = get_sysroots_path(&cli.config.twz_triple().to_string())?;
        for lib in [
            "libc.so",
            "libdl.so",
            "libpthread.so",
            "libm.so",
            "librt.so",
            "libc++abi.so",
        ] {
            let mut path = lib_path.clone();
            path.push(lib);
            initrd_files.push(path);
        }

        //initrd_files.push(testso_path);

        Ok(initrd_files)
    }
}

pub(crate) fn do_make_image(cli: ImageOptions) -> anyhow::Result<ImageInfo> {
    let comp = crate::build::do_build(cli.clone().into())?.unwrap();
    let triple = cli.config.twz_triple();
    let disk_image = crate::disk::image_path(&triple, cli.disk_image.as_deref());
    // Everything below writes state shared by every concurrent xtask. Taken after the build, which
    // cargo already serializes for itself and which is far too long to hold these over. Staging
    // first, then the image: two locks in one fixed order, so callers that need only one (the
    // `disk` subcommand, qemu's fresh-image path) cannot invert it.
    let _staging_lock = crate::imagelock::staging_lock(&triple, cli.config.profile)?;
    let _image_lock = crate::imagelock::image_lock(&disk_image)?;
    copy_twizzler_build(&comp, &triple, &disk_image)?;

    let initrd_files = build_initrd(&cli, &comp)?;
    let initrd_files = strip_initrd_files(&initrd_files, &comp)?;
    let data_files = generate_data_folder(&comp, cli.data.as_deref());
    let initrd_path = generate_initrd(initrd_files, data_files, &comp)?;

    let debug_sysroot = PathBuf::from("target/dynamic/")
        .join(cli.config.twz_triple().to_string())
        .join(cli.config.profile.to_string())
        .join("debug_sysroot");

    tracing::info!("debug sysroot at {}", debug_sysroot.display());
    std::fs::create_dir_all(debug_sysroot.join("initrd"))?;
    let mut tar = Command::new("tar");
    tar.args([
        "xf",
        &initrd_path.to_string_lossy(),
        "-C",
        &debug_sysroot.join("initrd").to_string_lossy(),
    ]);
    if !tar.status()?.success() {
        anyhow::bail!("failed to extract initrd image for debug sysroot");
    }

    let _ = std::fs::remove_file("target/debug_sysroot");
    std::os::unix::fs::symlink(
        debug_sysroot.strip_prefix("target").unwrap(),
        "target/debug_sysroot",
    )?;

    crate::print_status_line("disk image", Some(&cli.config));
    let mut cmdline = String::new();
    if cli.tests {
        cmdline.push_str("--tests ");
    }
    if cli.benches || cli.bench.is_some() {
        cmdline.push_str("--benches ");
    }
    // Before autostart, which is a bare program name and has to stay last.
    for arg in &cli.kernel_arg {
        cmdline.push_str(arg);
        cmdline.push(' ');
    }
    if let Some(autostart) = cli.autostart {
        cmdline.push_str(autostart.as_str());
    }

    let efi_binary = {
        let mut tc_path = get_toolchain_path()?;

        let name = match cli.config.arch {
            Arch::X86_64 => "BOOTX64.EFI",
            Arch::Aarch64 => "BOOTAA64.EFI",
        };

        tc_path.push(name);
        tc_path
    };

    let image_path = get_genfile_path(&comp, "disk.img");
    println!(
        "kernel: {:?}, cmdline: {}",
        comp.get_kernel_image(cli.tests || cli.benches || cli.bench.is_some()),
        cmdline
    );
    let status = Command::new(get_tool_path(&comp, "image_builder")?)
        .arg("--disk-path")
        .arg(&image_path)
        .arg("--kernel-path")
        .arg(comp.get_kernel_image(cli.tests || cli.benches || cli.bench.is_some()))
        .arg("--initrd-path")
        .arg(initrd_path)
        .arg(format!("--cmdline={}", cmdline.trim()))
        .arg("--efi-binary")
        .arg(efi_binary)
        .status()?;

    if status.success() {
        Ok(ImageInfo {
            disk_image: image_path,
        })
    } else {
        anyhow::bail!("failed to generate disk image");
    }
}
