//! Cache maintenance for DMA-written buffers, through the P4 ROM (the same calls esp-hal makes in
//! its private `soc::cache_invalidate_addr`, and ESP-IDF in `esp_cache_msync(.., DIR_M2C)`).

// `esp_rom/esp32p4/include/esp32p4/rom/cache.h`.
const CACHE_MAP_L1_DCACHE: u32 = 1 << 4;
const CACHE_MAP_L2_CACHE: u32 = 1 << 5;

/// Internal L2 memory (HP L2MEM, 768 KB). The CPU reaches it through the L1 data cache only;
/// PSRAM goes through L1 and L2.
const L2MEM: core::ops::Range<usize> = 0x4FF0_0000..0x4FFC_0000;

/// Offset of the non-cacheable alias of L2MEM (`SOC_NON_CACHEABLE_OFFSET_SRAM`, IDF
/// `CACHE_LL_L2MEM_NON_CACHE_ADDR`): CPU accesses there bypass the L1 cache.
const L2MEM_NON_CACHEABLE_OFFSET: usize = 0x4000_0000;

/// L1 data cache line (`cache_ll_l1_dcache_get_line_size`).
pub const L1_LINE: usize = 64;

unsafe extern "C" {
    fn Cache_Invalidate_Addr(map: u32, addr: u32, size: u32) -> i32;
    fn Cache_WriteBack_Addr(map: u32, addr: u32, size: u32) -> i32;
    fn Cache_Get_L2_Cache_Line_Size() -> u32;
}

/// Alignment (start and length) a DMA buffer at `addr` needs so that invalidating it never
/// touches a neighbour's cache line.
pub fn line_size(addr: usize) -> usize {
    if L2MEM.contains(&addr) {
        L1_LINE
    } else {
        // Set by the second-stage bootloader (64 or 128 B).
        let l2 = unsafe { Cache_Get_L2_Cache_Line_Size() } as usize;
        l2.max(L1_LINE)
    }
}

/// Drops every cached line of `addr..addr + len` so the CPU re-reads what the DMA wrote.
/// Dirty lines are discarded, not written back. `addr` and `len` must be multiples of
/// [`line_size`].
pub fn invalidate(addr: usize, len: usize) {
    let map = if L2MEM.contains(&addr) {
        CACHE_MAP_L1_DCACHE
    } else {
        CACHE_MAP_L1_DCACHE | CACHE_MAP_L2_CACHE
    };
    unsafe {
        Cache_Invalidate_Addr(map, addr as u32, len as u32);
    }
}

/// `addr..addr + len` lies in internal L2 memory.
pub fn in_l2mem(addr: usize, len: usize) -> bool {
    L2MEM.contains(&addr) && addr + len <= L2MEM.end
}

/// Non-cacheable CPU address of an L2MEM address (the DMA keeps using the cached one).
pub fn non_cacheable(addr: usize) -> usize {
    addr + L2MEM_NON_CACHEABLE_OFFSET
}

/// Writes back and then drops the cached lines of `addr..addr + len` (IDF
/// `esp_cache_msync(C2M | INVALIDATE)`), so that later non-cacheable accesses and DMA writes
/// are never overwritten by a stale dirty line. Same alignment rules as [`invalidate`].
pub fn writeback_invalidate(addr: usize, len: usize) {
    let map = if L2MEM.contains(&addr) {
        CACHE_MAP_L1_DCACHE
    } else {
        CACHE_MAP_L1_DCACHE | CACHE_MAP_L2_CACHE
    };
    unsafe {
        Cache_WriteBack_Addr(map, addr as u32, len as u32);
        Cache_Invalidate_Addr(map, addr as u32, len as u32);
    }
}
