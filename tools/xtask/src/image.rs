use std::{
    path::{Path, PathBuf},
    process::Command,
};

use crate::{build::TwizzlerCompilation, ImageOptions};

pub struct ImageInfo {
    pub disk_image: PathBuf,
}

fn get_crate_initrd_files(
    comp: &TwizzlerCompilation,
    crate_name: &str,
) -> anyhow::Result<Vec<PathBuf>> {
    let unit = comp
        .borrow_user_compilation()
        .binaries
        .iter()
        .find(|item| item.unit.pkg.name() == crate_name)
        .expect(&format!("failed to find initrd crate {}", crate_name));

    Ok(vec![unit.path.clone()])
}

fn get_tool_path<'a>(comp: &'a TwizzlerCompilation, name: &str) -> anyhow::Result<&'a Path> {
    let unit = comp
        .borrow_tools_compilation()
        .binaries
        .iter()
        .find(|item| item.unit.pkg.name() == name)
        .expect(&format!("failed to find initrd crate {}", name));
    Ok(&unit.path)
}

fn get_genfile_path(comp: &TwizzlerCompilation, name: &str) -> PathBuf {
    let mut path = comp.get_kernel_image().parent().unwrap().to_path_buf();
    path.push(name);
    path
}

fn build_initrd(cli: &ImageOptions, comp: &TwizzlerCompilation) -> anyhow::Result<PathBuf> {
    crate::print_status_line("initrd", Some(&cli.config));
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
        let split: Vec<_> = spec.split(":").into_iter().collect();
        if split.len() != 2 {
            anyhow::bail!("initrd item must be of the form `x:y'");
        }
        match split[0] {
            "crate" => initrd_files.append(&mut get_crate_initrd_files(comp, split[1])?),
            x => panic!("invalid initrd spec {}", x),
        }
    }
    let initrd_path = get_genfile_path(&comp, "initrd");
    let status = Command::new(get_tool_path(&comp, "initrd_gen")?)
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

pub(crate) fn do_make_image(cli: ImageOptions) -> anyhow::Result<ImageInfo> {
    let comp = crate::build::do_build(cli.into())?;
    let initrd_path = build_initrd(&cli, &comp)?;

    crate::print_status_line("disk image", Some(&cli.config));
    let cmdline = "";
    let image_path = get_genfile_path(&comp, "disk.img");
    let status = Command::new(get_tool_path(&comp, "image_builder")?)
        .arg(comp.get_kernel_image())
        .arg(initrd_path)
        .arg(cmdline)
        .status()?;

    if status.success() {
        Ok(ImageInfo {
            disk_image: image_path,
        })
    } else {
        anyhow::bail!("failed to generate disk image");
    }
    /*
    let test_bins = build_all(meta, args, build_info, build_options)?;
    let pkg_list: Vec<String> = meta.workspace_metadata["initrd-members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.to_string().replace("\"", ""))
        .collect();
    let mut initrd_files: Vec<String> = pkg_list
        .iter()
        .map(|x| make_path(build_info, false, x))
        .collect();
    if let Some(test_bins) = test_bins {
        for b in &test_bins {
            initrd_files.push(b.to_string());
        }
        let mut f = std::fs::File::create(make_path(build_info, true, "test_bins")).unwrap();
        let s = test_bins.iter().fold(String::new(), |mut x, y| {
            let path = Path::new(y).file_name().unwrap();
            x += &format!("{}\n", path.to_string_lossy());
            x
        });
        f.write_all(s.as_bytes()).unwrap();
        initrd_files.push(make_path(build_info, true, "test_bins"));
    }
    eprintln!("== BUILDING INITRD ({}) ==", build_info);
    let status = Command::new(make_tool_path(build_info, "initrd_gen"))
        .arg("--output")
        .arg(make_path(build_info, true, "initrd"))
        .args(&initrd_files)
        .status()?;
    if !status.success() {
        return Err("failed to generate initrd".into());
    }
    eprintln!("== BUILDING DISK IMAGE ({}) ==", build_info);
    let cmdline = if build_options.build_tests {
        "--tests"
    } else {
        ""
    };
    let status = Command::new(make_tool_path(build_info, "image_builder"))
        .arg(make_path(build_info, true, "twizzler-kernel"))
        .arg(make_path(build_info, true, "initrd"))
        .arg(cmdline)
        .status()?;

    if !status.success() {
        return Err("disk image creation failed".into());
    }
    */
}
