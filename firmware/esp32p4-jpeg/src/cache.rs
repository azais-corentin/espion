//! Cache maintenance through the P4 ROM (the calls `esp32p4-csi` and esp-hal make; ESP-IDF
//! `esp_cache_msync`).

// `esp_rom/esp32p4/include/esp32p4/rom/cache.h`.
const CACHE_MAP_L1_DCACHE: u32 = 1 << 4;
const CACHE_MAP_L2_CACHE: u32 = 1 << 5;

/// Internal L2 memory (HP L2MEM, 768 KB), cached by L1 only; PSRAM goes through L1 and L2.
const L2MEM: core::ops::Range<usize> = 0x4FF0_0000..0x4FFC_0000;

/// Offset of the non-cacheable alias of L2MEM (`SOC_NON_CACHEABLE_OFFSET_SRAM`).
const L2MEM_NON_CACHEABLE_OFFSET: usize = 0x4000_0000;

/// L1 data cache line.
const L1_LINE: usize = 64;

unsafe extern "C" {
    fn Cache_Invalidate_Addr(map: u32, addr: u32, size: u32) -> i32;
    fn Cache_WriteBack_Addr(map: u32, addr: u32, size: u32) -> i32;
    fn Cache_Get_L2_Cache_Line_Size() -> u32;
}

fn map(addr: usize) -> u32 {
    if L2MEM.contains(&addr) {
        CACHE_MAP_L1_DCACHE
    } else {
        CACHE_MAP_L1_DCACHE | CACHE_MAP_L2_CACHE
    }
}

/// Cache line of the memory at `addr`: 64 B in internal RAM, the larger of L1 and the L2 line
/// the bootloader set (64 or 128 B) elsewhere.
pub fn line_size(addr: usize) -> usize {
    if L2MEM.contains(&addr) {
        L1_LINE
    } else {
        // SAFETY: ROM getter, no side effects.
        (unsafe { Cache_Get_L2_Cache_Line_Size() } as usize).max(L1_LINE)
    }
}

/// Drops the cached lines of `addr..addr + len` (dirty ones are discarded). Both must be
/// multiples of [`line_size`].
pub fn invalidate(addr: usize, len: usize) {
    // SAFETY: ROM cache operation on a range the caller owns.
    unsafe { Cache_Invalidate_Addr(map(addr), addr as u32, len as u32) };
}

/// Writes the dirty cached lines of `addr..addr + len` back to memory. Partial lines at either
/// end are written back whole.
pub fn write_back(addr: usize, len: usize) {
    // SAFETY: as above; writing back never loses data.
    unsafe { Cache_WriteBack_Addr(map(addr), addr as u32, len as u32) };
}

/// `addr..addr + len` lies in internal L2 memory.
pub fn in_l2mem(addr: usize, len: usize) -> bool {
    L2MEM.contains(&addr) && addr + len <= L2MEM.end
}

/// Non-cacheable CPU address of an L2MEM address (the DMA keeps using the cached one).
pub fn non_cacheable(addr: usize) -> usize {
    addr + L2MEM_NON_CACHEABLE_OFFSET
}
