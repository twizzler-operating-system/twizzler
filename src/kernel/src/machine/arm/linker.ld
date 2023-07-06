/* linker script reference: https://github.com/limine-bootloader/limine/blob/trunk/test/linker.ld */
ENTRY(limine_entry)

/* Define the program headers we want so the bootloader gives us the right */
/* MMU permissions */
PHDRS
{
    null    PT_NULL    FLAGS(0) ;                   /* Null segment */
    text    PT_LOAD    FLAGS((1 << 0) | (1 << 2)) ; /* Execute + Read */
    rodata  PT_LOAD    FLAGS((1 << 2)) ;            /* Read only */
    data    PT_LOAD    FLAGS((1 << 1) | (1 << 2)) ; /* Write + Read */
    tls     PT_TLS     FLAGS((1 << 2)) ;            /* Read only */
}

SECTIONS
{
    /* The Limine spec mandates that we be placed in the topmost 2GiB of the address space */
    /* The image base is chosen by the configuration in .cargo/config.toml */

    .text : {
        *(.text .text.*)
    } :text

    /* Move to the next memory page for .rodata */
    . += CONSTANT(MAXPAGESIZE);
    
    .rodata : {
        *(.rodata .rodata.*)
    } :rodata

    /* Thread Local Storage sections  */
    .tdata : { 
            *(.tdata .tdata.*) 
    } :tls

    .tbss : {
        *(.tbss .tbss.*) 
        *(.tcommon) 
    } :tls

    .limine_reqs : {
        KEEP(*(.limine_reqs))
        QUAD(0)
    } :rodata

    /* Move to the next memory page for .data */
    . += CONSTANT(MAXPAGESIZE);

    .data : {
        *(.data .data.*)
    } :data

    .bss : {
        *(COMMON)
        *(.bss .bss.*)
    } :data
}
