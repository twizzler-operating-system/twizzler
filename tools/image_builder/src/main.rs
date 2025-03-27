use std::{
    convert::TryFrom,
    fs::{self, File},
    io::{self, Seek, Write},
    path::{Path, PathBuf},
};

use anyhow::{bail, Context};
use clap::Parser;

#[derive(Parser, Debug)]
struct Args {
    /// Path where disk image should be created
    #[clap(short, long)]
    disk_path: String,
    /// Path to kernel binary
    #[clap(short, long)]
    kernel_path: String,
    /// Path to initial ram disk
    #[clap(short, long)]
    initrd_path: String,
    /// Command line string to be passed to kernel
    #[clap(short, long)]
    cmdline: Vec<String>,
    /// EFI application binary used by bootloader
    #[clap(short, long)]
    efi_binary: String,
}

fn main() {
    let args = Args::parse();
    let disk_image_path = PathBuf::from(args.disk_path);
    let kernel_binary_path = {
        let path = PathBuf::from(args.kernel_path);
        path.canonicalize().unwrap()
    };
    let initrd_path = {
        let path = PathBuf::from(args.initrd_path);
        path.canonicalize().unwrap()
    };
    create_disk_images(
        &disk_image_path,
        &kernel_binary_path,
        &initrd_path,
        args.cmdline.join(" "),
        args.efi_binary,
    );
}

pub fn create_disk_images(
    disk_image_path: &Path,
    kernel_binary_path: &Path,
    initrd_path: &Path,
    cmdline: String,
    efi_binary: String,
) -> PathBuf {
    //let kernel_manifest_path = locate_cargo_manifest::locate_manifest().unwrap();
    //let kernel_binary_name = kernel_binary_path.file_name().unwrap().to_str().unwrap();
    if let Err(e) = create_uefi_disk_image(
        disk_image_path,
        kernel_binary_path,
        initrd_path,
        cmdline,
        efi_binary,
    ) {
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
    efi_binary: String,
) -> anyhow::Result<()> {
    let efi_file = Path::new(&efi_binary);
    let efi_size = fs::metadata(&efi_file)
        .context("failed to read metadata of efi file")?
        .len();
    let kernel_size = fs::metadata(&kernel_binary_path)
        .context("failed to read metadata of kernel file")?
        .len();
    let initrd_size = fs::metadata(&initrd_path)
        .context("failed to read metadata of initrd file")?
        .len();

    // limine.cfg file
    let cfg_data = format!(
        r#"
# Specifies the timeout in seconds before the first entry is automatically booted.
# If set to 0, boots default entry instantly (see default_entry option).
timeout: 1
# If set to yes, enable serial I/O for the bootloader.
serial: yes
# Print additional information during boot.
verbose: yes
# `default_entry` set to 1 by default (1-based index)

# The entry name that will be displayed in the boot menu.
/Twizzler
    # We use the Limine boot protocol.
    protocol: limine

    # Path to the kernel to boot. boot():/ represents the partition on which limine.conf is located.
    kernel_path: boot():/kernel.elf

    # The path to a module. This option can be specified multiple times to specify multiple modules.
    module_path: boot():/initrd

    # The command line string to be passed to the kernel/executable.
    kernel_cmdline: {}

    # The resolution to be used.
    resolution: 800x600"#,
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
        // use the same file name as the efi binary
        let boot_bin_path = Path::new("efi/boot/").join(efi_file.file_name().unwrap());
        let mut boot_bin = root_dir.create_file(boot_bin_path.as_path().to_str().unwrap())?;
        boot_bin.truncate()?;
        io::copy(&mut fs::File::open(&efi_file)?, &mut boot_bin)?;
        let mut kernel = root_dir.create_file("kernel.elf")?;
        kernel.truncate()?;
        io::copy(&mut fs::File::open(&kernel_binary_path)?, &mut kernel)?;
        let mut cfg = root_dir.create_file("limine.conf")?;
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
