//! 2D-DMA register block (TRM v0.7 chapter 8), only what the JPEG encoder uses.
//!
//! The `esp32p4` 0.3 PAC has no block for the 2D-DMA and esp-hal 1.2.2 has no driver for it.
//! ESP-IDF puts it at `DR_REG_DMA2D_BASE` = 0x5008_8000
//! (`soc/esp32p4/register/hw_ver3/soc/reg_base.h`). Offsets and bit positions are those of
//! ESP-IDF's `hw_ver3/soc/dma2d_reg.h` / `dma2d_struct.h` (chip revision v3.x) and match the
//! TRM's register summary (section 8.8) and register descriptions (section 8.9); the TRM
//! register number is given with each one. Layout:
//!
//! - TX ("out") channel `n` (0..=3 on v3.x): registers at `0x100 * n`;
//! - RX ("in") channel `n` (0..=2 on v3.x): registers at `0x500 + 0x100 * n`;
//! - common registers from 0xA00.
//!
//! To add a register: one more offset constant in [`out_reg`], [`in_reg`] or [`common`], with
//! its TRM number, and its bit constants next to it.

/// `DR_REG_DMA2D_BASE` (ESP-IDF `reg_base.h`; TRM table 9.3-2, "2D-DMA").
pub const BASE: usize = 0x5008_8000;

/// A 32-bit memory-mapped register.
#[derive(Clone, Copy)]
pub struct Reg(usize);

impl Reg {
    #[inline(always)]
    pub fn read(self) -> u32 {
        // SAFETY: `Reg` is only built from the offsets below, inside the 2D-DMA block.
        unsafe { core::ptr::read_volatile(self.0 as *const u32) }
    }

    #[inline(always)]
    pub fn write(self, value: u32) {
        // SAFETY: as in `read`.
        unsafe { core::ptr::write_volatile(self.0 as *mut u32, value) }
    }

    /// Read, clear `clear`, set `set`, write.
    #[inline(always)]
    pub fn modify(self, clear: u32, set: u32) {
        self.write((self.read() & !clear) | set);
    }
}

/// Register `offset` of TX channel `ch`.
#[inline(always)]
pub fn out(ch: u8, offset: usize) -> Reg {
    Reg(BASE + 0x100 * ch as usize + offset)
}

/// Register `offset` of RX channel `ch`.
#[inline(always)]
pub fn inp(ch: u8, offset: usize) -> Reg {
    Reg(BASE + 0x500 + 0x100 * ch as usize + offset)
}

/// Common register at `offset`.
#[inline(always)]
pub fn common(offset: usize) -> Reg {
    Reg(BASE + offset)
}

/// TX channel register offsets (relative to the channel's base) and fields.
pub mod out_reg {
    /// `DMA2D_OUT_CONF0_CHn_REG` (register 8.1).
    pub const CONF0: usize = 0x00;
    /// `DMA2D_OUT_INT_RAW_CHn_REG` (8.39).
    pub const INT_RAW: usize = 0x04;
    /// `DMA2D_OUT_INT_ENA_CHn_REG` (8.40).
    pub const INT_ENA: usize = 0x08;
    /// `DMA2D_OUT_INT_ST_CHn_REG` (8.41): raw bits masked by INT_ENA.
    pub const INT_ST: usize = 0x0C;
    /// `DMA2D_OUT_INT_CLR_CHn_REG` (8.42).
    pub const INT_CLR: usize = 0x10;
    /// `DMA2D_OUT_LINK_CONF_CHn_REG` (8.3).
    pub const LINK_CONF: usize = 0x1C;
    /// `DMA2D_OUT_LINK_ADDR_CHn_REG` (8.4).
    pub const LINK_ADDR: usize = 0x20;
    /// `DMA2D_OUT_STATE_CHn_REG` (8.48).
    pub const STATE: usize = 0x24;
    /// `DMA2D_OUT_EOF_DES_ADDR_CHn_REG` (8.49).
    pub const EOF_DES_ADDR: usize = 0x28;
    /// `DMA2D_OUT_PERI_SEL_CHn_REG` (8.62).
    pub const PERI_SEL: usize = 0x38;
    /// `DMA2D_OUT_COLOR_CONVERT_CHn_REG` (8.7).
    pub const COLOR_CONVERT: usize = 0x48;

    // CONF0 fields.
    /// `OUT_EOF_MODE`: TX EOF when the last byte has been popped from the FIFO.
    pub const EOF_MODE: u32 = 1 << 1;
    /// `OUTDSCR_BURST_EN`: INCR burst for descriptor reads.
    pub const DSCR_BURST_EN: u32 = 1 << 2;
    /// `OUT_CHECK_OWNER`.
    pub const CHECK_OWNER: u32 = 1 << 4;
    /// `OUT_MEM_BURST_LENGTH` (bits 8:6): 0 = 8 B .. 4 = 128 B.
    pub const BURST_SHIFT: u32 = 6;
    pub const BURST_MASK: u32 = 0x7 << BURST_SHIFT;
    /// `OUT_MACRO_BLOCK_SIZE` (bits 10:9): 0 = 8x8, 1 = 8x16, 2 = 16x16, 3 = none.
    pub const MB_SHIFT: u32 = 9;
    pub const MB_MASK: u32 = 0x3 << MB_SHIFT;
    /// `OUT_DSCR_PORT_EN` (PPA only).
    pub const DSCR_PORT_EN: u32 = 1 << 11;
    /// `OUT_PAGE_BOUND_EN`: AXI reads do not cross a burst-length boundary.
    pub const PAGE_BOUND_EN: u32 = 1 << 12;
    /// `OUT_REORDER_EN` (channel 0 only): macroblock reordering.
    pub const REORDER_EN: u32 = 1 << 16;
    /// `OUT_RST`: pulse to reset the channel's state machine and FIFO pointer.
    pub const RST: u32 = 1 << 24;
    /// `OUT_CMD_DISABLE`: set before a reset, clear after it.
    pub const CMD_DISABLE: u32 = 1 << 25;
    /// `OUT_AUTO_WRBACK` (bit 0).
    pub const AUTO_WRBACK: u32 = 1 << 0;

    // LINK_CONF fields.
    pub const LINK_STOP: u32 = 1 << 20;
    pub const LINK_START: u32 = 1 << 21;

    // STATE fields.
    /// `OUT_STATE` (bits 23:20): control state machine, 0 = idle.
    pub const STATE_SHIFT: u32 = 20;
    pub const STATE_MASK: u32 = 0xF << STATE_SHIFT;
    /// `OUT_RESET_AVAIL`: the channel may be reset now.
    pub const RESET_AVAIL: u32 = 1 << 24;

    // Interrupt bits (`dma2d_ll.h` `DMA2D_LL_EVENT_TX_*`, TRM 8.6).
    pub const INT_DONE: u32 = 1 << 0;
    pub const INT_EOF: u32 = 1 << 1;
    pub const INT_DSCR_ERR: u32 = 1 << 2;
    pub const INT_TOTAL_EOF: u32 = 1 << 3;
    /// FIFO L1..L3 overflow/underflow, reorder overflow/underflow.
    pub const INT_FIFO_ERR: u32 = 0xFF0;
    pub const INT_ALL: u32 = 0x1FFF;

    // COLOR_CONVERT fields: OUTPUT_SEL (1:0), 3B_PROC_EN (2), INPUT_SEL (5:3).
    /// No colour conversion: input 7 (disabled), output 2 (directly), as ESP-IDF's
    /// `dma2d_ll_tx_configure_color_space_conv(DMA2D_CSC_TX_NONE)`. The reset value has
    /// output 0 (RGB888 to RGB565).
    pub const CSC_NONE: u32 = (7 << 3) | 2;
}

/// RX channel register offsets (relative to the channel's base) and fields.
pub mod in_reg {
    /// `DMA2D_IN_CONF0_CHn_REG` (register 8.17).
    pub const CONF0: usize = 0x00;
    /// `DMA2D_IN_INT_RAW_CHn_REG` (8.43).
    pub const INT_RAW: usize = 0x04;
    /// `DMA2D_IN_INT_ENA_CHn_REG` (8.44).
    pub const INT_ENA: usize = 0x08;
    /// `DMA2D_IN_INT_ST_CHn_REG` (8.45).
    pub const INT_ST: usize = 0x0C;
    /// `DMA2D_IN_INT_CLR_CHn_REG` (8.46).
    pub const INT_CLR: usize = 0x10;
    /// `DMA2D_IN_LINK_CONF_CHn_REG` (8.19).
    pub const LINK_CONF: usize = 0x1C;
    /// `DMA2D_IN_LINK_ADDR_CHn_REG` (8.20).
    pub const LINK_ADDR: usize = 0x20;
    /// `DMA2D_IN_STATE_CHn_REG` (8.54).
    pub const STATE: usize = 0x24;
    /// `DMA2D_IN_SUC_EOF_DES_ADDR_CHn_REG` (8.55).
    pub const SUC_EOF_DES_ADDR: usize = 0x28;
    /// `DMA2D_IN_PERI_SEL_CHn_REG` (8.63).
    pub const PERI_SEL: usize = 0x3C;

    // CONF0 fields.
    /// `IN_MEM_TRANS_EN`: memory-to-memory (connects TX channel n to RX channel n).
    pub const MEM_TRANS_EN: u32 = 1 << 0;
    /// `INDSCR_BURST_EN`.
    pub const DSCR_BURST_EN: u32 = 1 << 2;
    /// `IN_CHECK_OWNER`.
    pub const CHECK_OWNER: u32 = 1 << 4;
    /// `IN_MEM_BURST_LENGTH` (bits 8:6), as for TX.
    pub const BURST_SHIFT: u32 = 6;
    pub const BURST_MASK: u32 = 0x7 << BURST_SHIFT;
    /// `IN_MACRO_BLOCK_SIZE` (bits 10:9), as for TX.
    pub const MB_SHIFT: u32 = 9;
    pub const MB_MASK: u32 = 0x3 << MB_SHIFT;
    pub const DSCR_PORT_EN: u32 = 1 << 11;
    pub const PAGE_BOUND_EN: u32 = 1 << 12;
    /// `IN_REORDER_EN` (channel 0 only).
    pub const REORDER_EN: u32 = 1 << 16;
    pub const RST: u32 = 1 << 24;
    pub const CMD_DISABLE: u32 = 1 << 25;

    // LINK_CONF fields.
    /// `INLINK_AUTO_RET`: value written back to the descriptor's owner bit (0 = CPU).
    pub const AUTO_RET: u32 = 1 << 20;
    pub const LINK_STOP: u32 = 1 << 21;
    pub const LINK_START: u32 = 1 << 22;

    // STATE fields.
    /// `IN_STATE` (bits 22:20), 0 = idle.
    pub const STATE_SHIFT: u32 = 20;
    pub const STATE_MASK: u32 = 0x7 << STATE_SHIFT;
    /// `IN_RESET_AVAIL`.
    pub const RESET_AVAIL: u32 = 1 << 23;

    // Interrupt bits (`dma2d_ll.h` `DMA2D_LL_EVENT_RX_*`, TRM 8.6).
    pub const INT_DONE: u32 = 1 << 0;
    /// Image received, `suc_eof` written back.
    pub const INT_SUC_EOF: u32 = 1 << 1;
    /// Image received with errors (JPEG only).
    pub const INT_ERR_EOF: u32 = 1 << 2;
    pub const INT_DSCR_ERR: u32 = 1 << 3;
    /// FIFO L1..L3 overflow/underflow (bits 4..=9).
    pub const INT_FIFO_ERR: u32 = 0x3F0;
    /// The descriptors' buffers are smaller than the data to be received.
    pub const INT_DSCR_EMPTY: u32 = 1 << 10;
    pub const INT_ALL: u32 = 0x3FFF;
}

/// Common registers.
pub mod common {
    /// `DMA2D_AXI_ERR_REG` (register 8.60): AXI error counters.
    pub const AXI_ERR: usize = 0xA00;
    /// `DMA2D_RST_CONF_REG` (8.32).
    pub const RST_CONF: usize = 0xA04;
    /// `DMA2D_INTR_MEM_START_ADDR_REG` (8.33), reset 0x3010_0000.
    pub const INTR_MEM_START: usize = 0xA08;
    /// `DMA2D_INTR_MEM_END_ADDR_REG` (8.34), reset 0x8FFF_FFFF.
    pub const INTR_MEM_END: usize = 0xA0C;
    /// `DMA2D_EXTR_MEM_START_ADDR_REG` (8.35), reset 0x3010_0000.
    pub const EXTR_MEM_START: usize = 0xA10;
    /// `DMA2D_EXTR_MEM_END_ADDR_REG` (8.36).
    pub const EXTR_MEM_END: usize = 0xA14;
    /// `DMA2D_OUT_ARB_CONFIG_REG` (8.37).
    pub const OUT_ARB_CONFIG: usize = 0xA18;
    /// `DMA2D_IN_ARB_CONFIG_REG` (8.38).
    pub const IN_ARB_CONFIG: usize = 0xA1C;
    /// `DMA2D_DATE_REG` (8.61): version, `dma2d_struct.h` default 0x0241_2190.
    pub const DATE: usize = 0xA2C;

    // RST_CONF fields.
    pub const AXIM_RD_RST: u32 = 1 << 0;
    pub const AXIM_WR_RST: u32 = 1 << 1;
    /// `DMA2D_CLK_EN`: register clock forced on.
    pub const CLK_EN: u32 = 1 << 2;
}

/// A 2D-DMA descriptor (TRM table 8.4-1; ESP-IDF `dma2d_descriptor_align8_t`). Five words;
/// the hardware needs it 8-byte aligned.
#[repr(C, align(8))]
#[derive(Clone, Copy, Default)]
pub struct Descriptor {
    /// DW0: `size[13:0]`/vb (13:0), `length[13:0]`/hb (27:14), err_eof (28), 2DEN (29),
    /// eof (30), owner (31).
    pub dw0: u32,
    /// DW1: `size[27:14]`/VA (13:0), `length[27:14]`/HA (27:14), pbyte (31:28).
    pub dw1: u32,
    /// DW2: Y (13:0), X (27:14), mod (28).
    pub dw2: u32,
    /// DW3: buffer address.
    pub buffer: u32,
    /// DW4: next descriptor, 0 for the last.
    pub next: u32,
}

/// DW0 `owner` = 1: the DMA may use the buffer.
pub const DW0_OWNER_DMA: u32 = 1 << 31;
/// DW0 `eof`.
pub const DW0_EOF: u32 = 1 << 30;
/// DW0 `2DEN`.
pub const DW0_2D: u32 = 1 << 29;
/// DW0 `err_eof` (RX, JPEG only).
pub const DW0_ERR_EOF: u32 = 1 << 28;
/// DW2 `mod` = 1: blocks of hb x vb until the HA x VA picture is done (2D-MOD1).
pub const DW2_MODE_MULTIPLE: u32 = 1 << 28;
/// pbyte field value for 1 byte per pixel.
pub const PBYTE_1: u32 = 1;
/// Largest value of a 14-bit descriptor field.
pub const FIELD_MAX: u32 = 0x3FFF;

impl Descriptor {
    /// A 2D-MOD1 transmit descriptor: the whole `ha` x `va` picture at `buffer`, 1 byte per
    /// pixel, read in blocks of `hb` x `vb` from (0, 0).
    pub const fn picture(buffer: u32, ha: u32, va: u32, hb: u32, vb: u32) -> Self {
        Self {
            dw0: DW0_OWNER_DMA | DW0_2D | (hb << 14) | vb,
            dw1: (PBYTE_1 << 28) | (ha << 14) | va,
            dw2: DW2_MODE_MULTIPLE,
            buffer,
            next: 0,
        }
    }

    /// A 1D receive descriptor for a buffer of `size` bytes (at most 2^28 - 1) at `buffer`.
    /// The hardware writes the received length and the eof bits back.
    pub const fn receive(buffer: u32, size: u32) -> Self {
        Self {
            dw0: DW0_OWNER_DMA | (size & FIELD_MAX),
            dw1: (PBYTE_1 << 28) | ((size >> 14) & FIELD_MAX),
            dw2: 0,
            buffer,
            next: 0,
        }
    }

    /// The length a 1D receive descriptor holds after the transfer.
    pub const fn received_len(dw0: u32, dw1: u32) -> u32 {
        (((dw1 >> 14) & FIELD_MAX) << 14) | ((dw0 >> 14) & FIELD_MAX)
    }
}
