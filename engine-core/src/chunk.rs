//! Chunk data storage: palette-compressed 16³ sections.
//!
//! Each section keeps a small palette of the unique block types it contains
//! plus a bit-packed index per block (`ceil(log2(palette_len))` bits, exact —
//! memory footprint is a project priority). Packing is non-straddling: an
//! index never crosses a `u64` word boundary (`64 / bits` entries per word),
//! trading a few wasted bits per word for shift/mask-only access with no
//! multi-word reads.
//!
//! The common uniform case (all air, all stone) is bits == 0: no index data
//! allocated at all, the section is just its 1-entry palette.
//!
//! Linear layout is y-major (`y*256 + z*16 + x`) so horizontal slices are
//! contiguous, matching the sweep order meshing will use.

/// A block type. Plain numeric id; what it *means* (hardness, solidity, …)
/// lives in the data-driven item/block registry, not here.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, PartialOrd, Ord)]
pub struct BlockId(pub u16);

pub const AIR: BlockId = BlockId(0);

/// Section edge length in blocks.
pub const SECTION_DIM: usize = 16;
/// Blocks per section (16³).
pub const SECTION_VOLUME: usize = SECTION_DIM * SECTION_DIM * SECTION_DIM;

/// Max index width: 4096 distinct blocks in 4096 cells.
const MAX_BITS: u8 = 12;

/// A palette-compressed 16³ block section.
#[derive(Clone, Debug)]
pub struct PalettedSection {
    /// Unique block types present (or once present — see [`Self::compact`]).
    /// `indices` values index into this. Never empty.
    palette: Vec<BlockId>,
    /// Bits per index. 0 iff the section is uniform (palette length 1, no data).
    bits: u8,
    /// Bit-packed indices, `64 / bits` per word, low bits first. Empty iff `bits == 0`.
    data: Vec<u64>,
}

impl PalettedSection {
    /// A section entirely filled with `block`. Allocation-free (uniform representation).
    pub fn filled(block: BlockId) -> Self {
        Self { palette: vec![block], bits: 0, data: Vec::new() }
    }

    #[inline]
    fn linear(x: usize, y: usize, z: usize) -> usize {
        debug_assert!(x < SECTION_DIM && y < SECTION_DIM && z < SECTION_DIM);
        (y * SECTION_DIM + z) * SECTION_DIM + x
    }

    #[inline]
    pub fn get(&self, x: usize, y: usize, z: usize) -> BlockId {
        self.palette[self.read_index(Self::linear(x, y, z))]
    }

    /// Sets one block, growing the palette (and index width) as needed.
    pub fn set(&mut self, x: usize, y: usize, z: usize, block: BlockId) {
        let cell = Self::linear(x, y, z);
        let pal_idx = match self.palette.iter().position(|&b| b == block) {
            Some(i) => i,
            None => {
                // Palette must grow; widen indices first if it's at capacity.
                if self.palette.len() == 1usize << self.bits {
                    self.repack(self.bits + 1);
                }
                self.palette.push(block);
                self.palette.len() - 1
            }
        };
        self.write_index(cell, pal_idx);
    }

    /// `Some(block)` if the whole section is provably one block type
    /// (uniform representation only — a section where every cell happens to
    /// hold palette entry 0 but `bits > 0` returns `None`; run [`Self::compact`]
    /// first if that distinction matters).
    pub fn uniform_block(&self) -> Option<BlockId> {
        (self.bits == 0).then(|| self.palette[0])
    }

    pub fn palette(&self) -> &[BlockId] {
        &self.palette
    }

    /// Heap bytes used by this section's storage (palette + packed indices).
    pub fn heap_bytes(&self) -> usize {
        self.palette.capacity() * std::mem::size_of::<BlockId>()
            + self.data.capacity() * std::mem::size_of::<u64>()
    }

    /// Rebuilds the palette to contain only block types actually referenced,
    /// shrinking index width (possibly back to uniform). `set` never removes
    /// palette entries — overwritten types linger until this is called. Call
    /// occasionally (e.g. before persisting or when memory pressure matters),
    /// not per-edit: it's an O(volume) pass.
    pub fn compact(&mut self) {
        if self.bits == 0 {
            return;
        }
        // Count usage per palette slot.
        let mut used = vec![false; self.palette.len()];
        for cell in 0..SECTION_VOLUME {
            used[self.read_index(cell)] = true;
        }
        let live = used.iter().filter(|&&u| u).count();
        debug_assert!(live >= 1);
        if live == self.palette.len() && bits_for(live) == self.bits {
            return; // Already minimal.
        }

        // old palette index -> new palette index; build the pruned palette.
        let mut remap = vec![usize::MAX; self.palette.len()];
        let mut new_palette = Vec::with_capacity(live);
        for (old_idx, &block) in self.palette.iter().enumerate() {
            if used[old_idx] {
                remap[old_idx] = new_palette.len();
                new_palette.push(block);
            }
        }

        let new_bits = bits_for(new_palette.len());
        if new_bits == 0 {
            *self = Self::filled(new_palette[0]);
            return;
        }
        let mut new = Self {
            palette: new_palette,
            bits: new_bits,
            data: vec![0; packed_len(new_bits)],
        };
        for cell in 0..SECTION_VOLUME {
            new.write_index(cell, remap[self.read_index(cell)]);
        }
        *self = new;
    }

    /// Re-encodes index storage at `new_bits` width (palette unchanged).
    fn repack(&mut self, new_bits: u8) {
        assert!(new_bits <= MAX_BITS, "palette cannot exceed section volume");
        debug_assert!(new_bits > self.bits);
        let mut new_data = vec![0u64; packed_len(new_bits)];
        if self.bits > 0 {
            let (old_bits, old_data) = (self.bits, std::mem::take(&mut self.data));
            let old_per_word = 64 / old_bits as usize;
            let old_mask = mask(old_bits);
            let new_per_word = 64 / new_bits as usize;
            for cell in 0..SECTION_VOLUME {
                let idx = (old_data[cell / old_per_word]
                    >> ((cell % old_per_word) as u32 * old_bits as u32))
                    & old_mask;
                new_data[cell / new_per_word] |=
                    idx << ((cell % new_per_word) as u32 * new_bits as u32);
            }
        }
        // bits == 0: every cell is palette index 0, i.e. all-zero packed words —
        // freshly zeroed new_data is already correct.
        self.bits = new_bits;
        self.data = new_data;
    }

    #[inline]
    fn read_index(&self, cell: usize) -> usize {
        debug_assert!(cell < SECTION_VOLUME);
        if self.bits == 0 {
            return 0;
        }
        let per_word = 64 / self.bits as usize;
        let word = self.data[cell / per_word];
        ((word >> ((cell % per_word) as u32 * self.bits as u32)) & mask(self.bits)) as usize
    }

    #[inline]
    fn write_index(&mut self, cell: usize, pal_idx: usize) {
        debug_assert!(cell < SECTION_VOLUME);
        debug_assert!(pal_idx < self.palette.len());
        if self.bits == 0 {
            // Uniform section and pal_idx must be 0 (the only entry): no-op.
            debug_assert_eq!(pal_idx, 0);
            return;
        }
        let per_word = 64 / self.bits as usize;
        let shift = (cell % per_word) as u32 * self.bits as u32;
        let word = &mut self.data[cell / per_word];
        *word = (*word & !(mask(self.bits) << shift)) | ((pal_idx as u64) << shift);
    }
}

#[inline]
fn mask(bits: u8) -> u64 {
    (1u64 << bits) - 1
}

/// Packed word count for `SECTION_VOLUME` entries at `bits` width (non-straddling).
fn packed_len(bits: u8) -> usize {
    let per_word = 64 / bits as usize;
    SECTION_VOLUME.div_ceil(per_word)
}

/// Minimum index width for a palette of `len` entries (0 = uniform).
fn bits_for(len: usize) -> u8 {
    debug_assert!(len >= 1);
    (usize::BITS - (len - 1).leading_zeros()) as u8
}

/// A vertical column of sections, sparse in y to support vertical streaming:
/// only generated slices exist; anything absent is implicitly `missing_as`
/// (air above ground, ungenerated below — callers decide meaning by context).
#[derive(Debug, Default)]
pub struct ChunkColumn {
    sections: std::collections::BTreeMap<i32, PalettedSection>,
}

impl ChunkColumn {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn section(&self, section_y: i32) -> Option<&PalettedSection> {
        self.sections.get(&section_y)
    }

    pub fn section_mut(&mut self, section_y: i32) -> Option<&mut PalettedSection> {
        self.sections.get_mut(&section_y)
    }

    /// Inserts (or replaces) a section, returning the old one if any.
    pub fn insert_section(
        &mut self,
        section_y: i32,
        section: PalettedSection,
    ) -> Option<PalettedSection> {
        self.sections.insert(section_y, section)
    }

    pub fn remove_section(&mut self, section_y: i32) -> Option<PalettedSection> {
        self.sections.remove(&section_y)
    }

    /// Block accessor across sections; `y` is a world-space block y.
    /// Returns `None` when the containing section isn't loaded/generated.
    pub fn get(&self, x: usize, y: i32, z: usize) -> Option<BlockId> {
        let section = self.sections.get(&y.div_euclid(SECTION_DIM as i32))?;
        Some(section.get(x, y.rem_euclid(SECTION_DIM as i32) as usize, z))
    }

    pub fn heap_bytes(&self) -> usize {
        self.sections.values().map(PalettedSection::heap_bytes).sum()
    }

    pub fn loaded_sections(&self) -> impl Iterator<Item = (i32, &PalettedSection)> {
        self.sections.iter().map(|(&y, s)| (y, s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uniform_is_allocation_free() {
        let s = PalettedSection::filled(AIR);
        assert_eq!(s.uniform_block(), Some(AIR));
        assert_eq!(s.get(0, 0, 0), AIR);
        assert_eq!(s.get(15, 15, 15), AIR);
        assert!(s.data.is_empty());
    }

    #[test]
    fn set_same_block_stays_uniform() {
        let mut s = PalettedSection::filled(BlockId(7));
        s.set(3, 4, 5, BlockId(7));
        assert_eq!(s.uniform_block(), Some(BlockId(7)));
        assert!(s.data.is_empty());
    }

    #[test]
    fn single_edit_roundtrip() {
        let mut s = PalettedSection::filled(AIR);
        s.set(1, 2, 3, BlockId(42));
        assert_eq!(s.get(1, 2, 3), BlockId(42));
        assert_eq!(s.get(0, 0, 0), AIR);
        assert_eq!(s.uniform_block(), None);
        assert_eq!(s.bits, 1);
    }

    #[test]
    fn palette_growth_across_bit_widths() {
        let mut s = PalettedSection::filled(AIR);
        // 300 distinct block types forces bits 1→2→…→9 growth via repack.
        for i in 0..300usize {
            let (x, y, z) = (i % 16, i / 256, (i / 16) % 16);
            s.set(x, y, z, BlockId(1000 + i as u16));
        }
        assert_eq!(s.bits, 9); // 301 palette entries (incl. AIR) → 9 bits.
        for i in 0..300usize {
            let (x, y, z) = (i % 16, i / 256, (i / 16) % 16);
            assert_eq!(s.get(x, y, z), BlockId(1000 + i as u16), "cell {i}");
        }
        // Untouched cells still read AIR after all the repacking.
        assert_eq!(s.get(15, 15, 15), AIR);
    }

    #[test]
    fn full_volume_distinct_writes() {
        let mut s = PalettedSection::filled(AIR);
        for y in 0..SECTION_DIM {
            for z in 0..SECTION_DIM {
                for x in 0..SECTION_DIM {
                    // 128 distinct types scattered over every cell.
                    let id = ((x * 31 + z * 7 + y * 13) % 128) as u16;
                    s.set(x, y, z, BlockId(id));
                }
            }
        }
        for y in 0..SECTION_DIM {
            for z in 0..SECTION_DIM {
                for x in 0..SECTION_DIM {
                    let id = ((x * 31 + z * 7 + y * 13) % 128) as u16;
                    assert_eq!(s.get(x, y, z), BlockId(id));
                }
            }
        }
    }

    #[test]
    fn fuzz_against_dense_mirror() {
        // Deterministic LCG so the test needs no rand dependency.
        let mut state = 0x2545F4914F6CDD1Du64;
        let mut rng = move || {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (state >> 33) as usize
        };
        let mut s = PalettedSection::filled(AIR);
        let mut mirror = [AIR; SECTION_VOLUME];
        for _ in 0..20_000 {
            let (x, y, z) = (rng() % 16, rng() % 16, rng() % 16);
            let block = BlockId((rng() % 40) as u16); // enough types to cross widths
            s.set(x, y, z, block);
            mirror[(y * 16 + z) * 16 + x] = block;
        }
        for y in 0..16 {
            for z in 0..16 {
                for x in 0..16 {
                    assert_eq!(s.get(x, y, z), mirror[(y * 16 + z) * 16 + x]);
                }
            }
        }
    }

    #[test]
    fn compact_prunes_and_can_return_to_uniform() {
        let mut s = PalettedSection::filled(AIR);
        for i in 0..40u16 {
            s.set(i as usize % 16, 0, 0, BlockId(i + 1));
        }
        assert!(s.palette.len() > 16);
        // Overwrite everything back to stone.
        for y in 0..16 {
            for z in 0..16 {
                for x in 0..16 {
                    s.set(x, y, z, BlockId(99));
                }
            }
        }
        s.compact();
        assert_eq!(s.uniform_block(), Some(BlockId(99)));
        assert!(s.data.is_empty());
        assert_eq!(s.palette.len(), 1);
    }

    #[test]
    fn compact_partial_prune_preserves_content() {
        let mut s = PalettedSection::filled(AIR);
        for i in 0..20u16 {
            s.set(i as usize % 16, (i / 16) as usize, 0, BlockId(100 + i));
        }
        // Erase most of them back to air, leaving 3 distinct non-air types.
        for i in 0..17u16 {
            s.set(i as usize % 16, (i / 16) as usize, 0, AIR);
        }
        let before: Vec<_> = (0..16)
            .flat_map(|y| (0..16).flat_map(move |z| (0..16).map(move |x| (x, y, z))))
            .map(|(x, y, z)| s.get(x, y, z))
            .collect();
        s.compact();
        let after: Vec<_> = (0..16)
            .flat_map(|y| (0..16).flat_map(move |z| (0..16).map(move |x| (x, y, z))))
            .map(|(x, y, z)| s.get(x, y, z))
            .collect();
        assert_eq!(before, after);
        assert_eq!(s.palette.len(), 4); // AIR + 3 survivors
        assert_eq!(s.bits, 2);
    }

    #[test]
    fn memory_footprint_sane() {
        // Two-type section: 1 bit/block → 64 words → 512 bytes of index data.
        let mut s = PalettedSection::filled(AIR);
        for x in 0..16 {
            s.set(x, 0, 0, BlockId(1));
        }
        assert_eq!(s.data.len(), SECTION_VOLUME / 64);
        assert!(s.heap_bytes() < 700, "got {}", s.heap_bytes());
    }

    #[test]
    fn column_sparse_sections_and_world_y() {
        let mut c = ChunkColumn::new();
        c.insert_section(0, PalettedSection::filled(BlockId(1)));
        c.insert_section(-2, PalettedSection::filled(BlockId(2)));
        assert_eq!(c.get(0, 5, 0), Some(BlockId(1)));
        assert_eq!(c.get(0, -17, 0), Some(BlockId(2))); // y −17 → section −2, local 15
        assert_eq!(c.get(0, 16, 0), None); // section 1 not loaded
        assert_eq!(c.get(0, -1, 0), None); // section −1 not loaded
        let mut sec_ys: Vec<i32> = c.loaded_sections().map(|(y, _)| y).collect();
        sec_ys.sort_unstable();
        assert_eq!(sec_ys, vec![-2, 0]);
    }

    #[test]
    fn bits_for_widths() {
        assert_eq!(bits_for(1), 0);
        assert_eq!(bits_for(2), 1);
        assert_eq!(bits_for(3), 2);
        assert_eq!(bits_for(4), 2);
        assert_eq!(bits_for(5), 3);
        assert_eq!(bits_for(256), 8);
        assert_eq!(bits_for(257), 9);
        assert_eq!(bits_for(4096), 12);
    }
}
