pub fn install(triple: &Triple) -> anyhow::Result<()> {
    println!("Building OpenSSL for {}", triple);

    Ok(())
}


let data = r#"
    my %targets = (
    "twizzler-common" => {
        template         => 1,
        CC               => "/scratch/dbittman/review/twizzler/toolchain/install/bin/clang",
        CFLAGS           => add_before(picker(default => "-Wall",
                                              debug   => "-g -O0",
                                              release => "-O2")),
        cflags           => add_before("-DL_ENDIAN",
                                       threads("-D_REENTRANT")),
        AR              => "llvm-ar",
        ARFLAGS         => "qc",
        HASHBANGPERL    => "/bin/env perl",
        sys_id           => "TWIZZLER",
        ex_libs          => "",
        perlasm_scheme   => "elf",
        thread_scheme    => "pthreads",
        dso_scheme       => "dlfcn",
        shared_target    => "gnu-shared",
        shared_cflag     => "-fPIC",
        shared_ldflag    => "-shared",
        perl_platform    => 'Unix',
    },
    "twizzler-x86_64" => {
        inherit_from     => [ "twizzler-common" ],
        cflags           => add("-target x86_64-unknown-twizzler --sysroot /scratch/dbittman/review/twizzler/toolchain/install/sysroots/x86_64-unknown-twizzler"),
        bn_ops           => "SIXTY_FOUR_BIT_LONG",
    },
);"#;