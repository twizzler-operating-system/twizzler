use std::path::PathBuf;

use crate::ImageOptions;

pub struct ImageInfo {
    pub disk_image: PathBuf,
}

pub(crate) fn do_make_image(cli: ImageOptions) -> anyhow::Result<ImageInfo> {
    let comp = crate::build::do_build(cli.into())?;

    todo!()
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
