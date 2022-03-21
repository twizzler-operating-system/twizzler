use anyhow::{bail, Context};
use std::{
    convert::TryFrom,
    fs::{self, File},
    io::{self, Seek, Write},
    path::{Path, PathBuf},
};

fn main() {
    let mut args = std::env::args().skip(1); // skip executable name

    /* TODO: better args processing */
    let disk_image_path = {
        let path = PathBuf::from(args.next().unwrap());
        path
    };
    let kernel_binary_path = {
        let path = PathBuf::from(args.next().unwrap());
        path.canonicalize().unwrap()
    };
    let initrd_path = {
        let path = PathBuf::from(args.next().unwrap());
        path.canonicalize().unwrap()
    };
    let cmdline = args.next().unwrap_or(String::new());
    create_disk_images(&disk_image_path, &kernel_binary_path, &initrd_path, cmdline);
}

pub fn create_disk_images(
    disk_image_path: &Path,
    kernel_binary_path: &Path,
    initrd_path: &Path,
    cmdline: String,
) -> PathBuf {
    //let kernel_manifest_path = locate_cargo_manifest::locate_manifest().unwrap();
    //let kernel_binary_name = kernel_binary_path.file_name().unwrap().to_str().unwrap();
    if let Err(e) =
        create_uefi_disk_image(disk_image_path, kernel_binary_path, initrd_path, cmdline)
    {
        panic!("failed to create disk image: {:?}", e);
    }
    if !disk_image_path.exists() {
        panic!(
            "Disk image does not exist at {} after bootloader build",
            disk_image_path.display()
        );
    }
    disk_image_path.to_path_buf()
}

fn create_uefi_disk_image(
    disk_image_path: &Path,
    kernel_binary_path: &Path,
    initrd_path: &Path,
    cmdline: String,
) -> anyhow::Result<()> {
    let efi_file = Path::new("toolchain/install/BOOTX64.EFI");
    let efi_size = fs::metadata(&efi_file)
        .context("failed to read metadata of efi file")?
        .len();
    let kernel_size = fs::metadata(&kernel_binary_path)
        .context("failed to read metadata of kernel file")?
        .len();
    let initrd_size = fs::metadata(&initrd_path)
        .context("failed to read metadata of initrd file")?
        .len();

    let cfg_data = format!(
        r#"
TIMEOUT=1 
DEFAULT_ENTRY=1
:Twizzler
RESOLUTION=800x600
PROTOCOL=stivale2
KERNEL_PATH=boot:///kernel.elf
MODULE_PATH=boot:///initrd
KERNEL_CMDLINE={}
"#,
        cmdline
    );
    // create fat partition
    let fat_file_path = {
        const MB: u64 = 1024 * 1024;

        let fat_path = disk_image_path.parent().unwrap().join("image.fat");
        let fat_file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&fat_path)
            .context("Failed to create UEFI FAT file")?;
        let efi_size_padded_and_rounded = ((efi_size + 1024 * 64 - 1) / MB + 1) * MB;
        let kernel_size_padded_and_rounded = ((kernel_size + 1024 * 64 - 1) / MB + 1) * MB;
        let cfg_size_padded_and_rounded = ((cfg_data.len() as u64 + 1024 * 64 - 1) / MB + 1) * MB;
        let initrd_size_padded_and_rounded = ((initrd_size + 1024 * 64 - 1) / MB + 1) * MB;
        fat_file
            .set_len(
                efi_size_padded_and_rounded
                    + kernel_size_padded_and_rounded
                    + initrd_size_padded_and_rounded
                    + cfg_size_padded_and_rounded,
            )
            .context("failed to set UEFI FAT file length")?;

        // create new FAT partition
        let format_options = fatfs::FormatVolumeOptions::new().volume_label(*b"FOOO       ");
        fatfs::format_volume(&fat_file, format_options)
            .context("Failed to format UEFI FAT file")?;

        // copy EFI file to FAT filesystem
        let partition = fatfs::FileSystem::new(&fat_file, fatfs::FsOptions::new())
            .context("Failed to open FAT file system of UEFI FAT file")?;
        let root_dir = partition.root_dir();
        root_dir.create_dir("efi")?;
        root_dir.create_dir("efi/boot")?;
        let mut bootx64 = root_dir.create_file("efi/boot/bootx64.efi")?;
        bootx64.truncate()?;
        io::copy(&mut fs::File::open(&efi_file)?, &mut bootx64)?;
        let mut kernel = root_dir.create_file("kernel.elf")?;
        kernel.truncate()?;
        io::copy(&mut fs::File::open(&kernel_binary_path)?, &mut kernel)?;
        let mut cfg = root_dir.create_file("limine.cfg")?;
        cfg.write_all(cfg_data.as_bytes())?;
        let mut initrd = root_dir.create_file("initrd")?;
        initrd.truncate()?;
        io::copy(&mut fs::File::open(&initrd_path)?, &mut initrd)?;

        fat_path
    };

    // create gpt disk
    {
        let image_path = disk_image_path;
        let mut image = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(&image_path)
            .context("failed to create UEFI disk image")?;

        let partition_size: u64 = fs::metadata(&fat_file_path)
            .context("failed to read metadata of UEFI FAT partition")?
            .len();
        let image_size = partition_size + 1024 * 64;
        image
            .set_len(image_size)
            .context("failed to set length of UEFI disk image")?;

        // Create a protective MBR at LBA0
        let mbr = gpt::mbr::ProtectiveMBR::with_lb_size(
            u32::try_from((image_size / 512) - 1).unwrap_or(0xFF_FF_FF_FF),
        );
        mbr.overwrite_lba0(&mut image)
            .context("failed to write protective MBR")?;

        // create new GPT in image file
        let block_size = gpt::disk::LogicalBlockSize::Lb512;
        let block_size_bytes: u64 = block_size.into();
        let mut disk = gpt::GptConfig::new()
            .writable(true)
            .initialized(false)
            .logical_block_size(block_size)
            .create_from_device(Box::new(&mut image), None)
            .context("failed to open UEFI disk image")?;
        disk.update_partitions(Default::default())
            .context("failed to initialize GPT partition table")?;

        // add add EFI system partition
        let partition_id = disk
            .add_partition("boot", partition_size, gpt::partition_types::EFI, 0, None)
            .context("failed to add boot partition")?;

        let partition = disk
            .partitions()
            .get(&partition_id)
            .ok_or_else(|| anyhow::anyhow!("Partition doesn't exist after adding it"))?;
        let created_partition_size: u64 =
            (partition.last_lba - partition.first_lba + 1u64) * block_size_bytes;
        if created_partition_size != partition_size {
            bail!(
                "Created partition has invalid size (size is {:?}, expected {})",
                created_partition_size,
                partition_size
            );
        }
        let start_offset = partition
            .bytes_start(block_size)
            .context("failed to retrieve partition start offset")?;

        // Write the partition table
        disk.write()
            .context("failed to write GPT partition table to UEFI image file")?;

        image
            .seek(io::SeekFrom::Start(start_offset))
            .context("failed to seek to boot partiiton start")?;
        let bytes_written = io::copy(
            &mut File::open(&fat_file_path).context("failed to open fat image")?,
            &mut image,
        )
        .context("failed to write boot partition content")?;
        if bytes_written != partition_size {
            bail!(
                "Invalid number of partition bytes written (expected {}, got {})",
                partition_size,
                bytes_written
            );
        }
    }

    Ok(())
}
