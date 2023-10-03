use std::{
    fs::File,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::Context;
use cargo::core::compiler::Compilation;

use crate::{build::TwizzlerCompilation, triple::Arch, ImageOptions};

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

fn build_initrd(cli: &ImageOptions, comp: &TwizzlerCompilation) -> anyhow::Result<PathBuf> {
    crate::print_status_line("initrd", Some(&cli.config));
    // Create an empty initrd if we are building just the kernel.
    // No user space components are required to run the code, but
    // the bootloader (limine) is configured to expect initrd,
    // even if we are not going to use it.
    if cli.kernel {
        let initrd_path = get_genfile_path(comp, "initrd");
        let result = File::create(&initrd_path);
        if result.is_ok() {
            Ok(initrd_path)
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
                "crate" => initrd_files.append(&mut get_crate_initrd_files(comp, split[1])?),
                "third-party" => {
                    initrd_files.append(&mut get_third_party_initrd_files(comp, split[1])?)
                }
                x => panic!("invalid initrd spec {}", x),
            }
        }

        if let Some(ref test_comp) = comp.borrow_test_compilation() {
            if cli.tests {
                let mut testlist = String::new();
                for bin in test_comp.tests.iter() {
                    initrd_files.push(bin.path.clone());
                    testlist += &bin.path.file_name().unwrap().to_string_lossy();
                    testlist += "\n";
                }
                let test_file_path = get_genfile_path(comp, "test_bins");
                let mut file = File::create(&test_file_path)?;
                file.write_all(testlist.as_bytes())?;
                initrd_files.push(test_file_path);
            }
        } else {
            assert!(!cli.tests);
        }

        let initrd_path = get_genfile_path(comp, "initrd");
        let status = Command::new(get_tool_path(comp, "initrd_gen")?)
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
}

pub(crate) fn do_make_image(cli: ImageOptions) -> anyhow::Result<ImageInfo> {
    let comp = crate::build::do_build(cli.into())?;
    let initrd_path = build_initrd(&cli, &comp)?;

    crate::print_status_line("disk image", Some(&cli.config));
    let cmdline = if cli.tests { "--tests" } else { "" };
    let efi_binary = match cli.config.arch {
        Arch::X86_64 => "toolchain/install/BOOTX64.EFI",
        Arch::Aarch64 => "toolchain/install/BOOTAA64.EFI",
    };
    let image_path = get_genfile_path(&comp, "disk.img");
    println!("kernel: {:?}", comp.get_kernel_image(cli.tests));
    let status = Command::new(get_tool_path(&comp, "image_builder")?)
        .arg("--disk-path")
        .arg(&image_path)
        .arg("--kernel-path")
        .arg(comp.get_kernel_image(cli.tests))
        .arg("--initrd-path")
        .arg(initrd_path)
        .arg(format!("--cmdline={}", cmdline))
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
