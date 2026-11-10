/* Buffers esp-hal's DMA-capable wrappers (`InternalMemory::get_mut`) accept.
 *
 * esp-hal 1.2.2 asserts that such a buffer lies in the "DRAM" range of esp-metadata-generated
 * 0.5.3, 0x4FF40000..0x4FFADFC0, which assumes a 256 KiB L2 cache. This firmware runs with the
 * 128 KiB the bootloader programs (`ESP_HAL_CONFIG_L2_CACHE_SIZE`), so RAM starts at 0x4FF20000
 * and a static in .bss may land below 0x4FF40000 and fail the assertion (the Ethernet driver
 * checks every descriptor and buffer access). This section goes right before the stack, at
 * 0x4FF40000 or higher. NOLOAD: the code zeroes what it places here before use.
 */
SECTIONS {
  .dram_dma MAX(ALIGN(ADDR(.noinit) + SIZEOF(.noinit), 64), 0x4FF40000) (NOLOAD) :
  {
    *(.dram_dma .dram_dma.*)
  } > RWDATA
}
INSERT BEFORE .stack;
