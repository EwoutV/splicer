//! Data-segment packing helpers for tier-2. Name-keyed record writes
//! over a schema-derived [`RecordLayout`], typed [`BlobSlice`]
//! pointer/length pairs, and a [`Segment`] / [`SymRef`] / [`Reloc`]
//! relocation model so segment placement order is commutative.

use std::collections::HashMap;

use super::super::abi::emit::{
    BlobSlice, RecordLayout, OPTION_NONE, OPTION_SOME, SLICE_LEN_OFFSET, SLICE_PTR_OFFSET,
};

/// Append-only string interner; the only way to obtain a `BlobSlice`
/// is `intern`, the only way to surface bytes is `into_bytes`. Repeat
/// `intern` of the same string returns the same slice (dedups).
pub(crate) struct NameInterner {
    bytes: Vec<u8>,
    seen: HashMap<String, BlobSlice>,
}

impl NameInterner {
    pub(crate) fn new() -> Self {
        Self {
            bytes: Vec::new(),
            seen: HashMap::new(),
        }
    }

    /// Append `s` to the blob if not already present, returning the
    /// `(offset, len)` slice for it.
    pub(crate) fn intern(&mut self, s: &str) -> BlobSlice {
        if let Some(&slice) = self.seen.get(s) {
            return slice;
        }
        let slice = BlobSlice {
            off: self.bytes.len() as u32,
            len: s.len() as u32,
        };
        self.bytes.extend_from_slice(s.as_bytes());
        self.seen.insert(s.to_string(), slice);
        slice
    }

    pub(crate) fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

/// Names a future data-segment base address.
pub(crate) type SymbolId = u32;

/// One pending pointer write. After segments have bases, layout writes
/// `bases[target] + addend` as LE i32 at `segment_base + site`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Reloc {
    pub(crate) site: u32,
    pub(crate) target: SymbolId,
    pub(crate) addend: i32,
}

/// One bytes-and-relocs unit handed to the layout phase.
pub(crate) struct Segment {
    pub(crate) id: SymbolId,
    pub(crate) align: u32,
    pub(crate) bytes: Vec<u8>,
    pub(crate) relocs: Vec<Reloc>,
}

/// A `(ptr, len)` pair into segment `target` at relative `off`.
/// `resolve` consumes the symbolic form (typed "translate twice" check).
/// `None` resolves to `BlobSlice::EMPTY`.
#[derive(Clone, Copy, Debug)]
pub(super) struct SymRef {
    pub(super) target: SymbolId,
    pub(super) off: u32,
    pub(super) len: u32,
}

/// Resolve an optional [`SymRef`] to an absolute [`BlobSlice`]. `None`
/// maps to [`BlobSlice::EMPTY`]; `Some` looks the target up in
/// `symbols` and adds `off`.
pub(super) fn resolve(sym: Option<SymRef>, symbols: &SymbolBases) -> BlobSlice {
    match sym {
        None => BlobSlice::EMPTY,
        Some(s) => BlobSlice {
            off: symbols.base_of(s.target) + s.off,
            len: s.len,
        },
    }
}

/// One assigned base address per [`SymbolId`]. Linker-side "where did
/// symbol N land?" only — no names, types, or scopes.
pub(super) struct SymbolBases {
    bases: Vec<Option<u32>>,
}

impl SymbolBases {
    pub(super) fn new() -> Self {
        Self { bases: Vec::new() }
    }

    pub(super) fn alloc(&mut self) -> SymbolId {
        let id = self.bases.len() as SymbolId;
        self.bases.push(None);
        id
    }

    pub(super) fn set(&mut self, id: SymbolId, base: u32) {
        let prev = self.bases[id as usize].replace(base);
        debug_assert!(prev.is_none(), "symbol {id} placed twice");
    }

    pub(super) fn base_of(&self, id: SymbolId) -> u32 {
        self.bases[id as usize].expect("symbol queried before placement")
    }
}

/// Defers reloc resolution until every target symbol has a base. The
/// whole point of this layer — placing segments in any order produces
/// the same final bytes.
pub(super) struct RelocPlan {
    pending: Vec<PendingReloc>,
}

struct PendingReloc {
    /// Index into `data_segments`; captured so resolve skips the scan.
    seg_idx: usize,
    /// Absolute byte offset of the 4-byte slot to overwrite.
    site: u32,
    target: SymbolId,
    addend: i32,
}

impl RelocPlan {
    pub(super) fn new() -> Self {
        Self {
            pending: Vec::new(),
        }
    }

    /// Caller must have already registered the segment's symbol via
    /// `SymbolBases::set`.
    pub(super) fn record_segment(&mut self, seg_idx: usize, seg_base: u32, relocs: Vec<Reloc>) {
        for r in relocs {
            self.pending.push(PendingReloc {
                seg_idx,
                site: seg_base + r.site,
                target: r.target,
                addend: r.addend,
            });
        }
    }

    pub(super) fn resolve(self, symbols: &SymbolBases, data_segments: &mut [(u32, Vec<u8>)]) {
        for r in self.pending {
            let value = (symbols.base_of(r.target) as i32).wrapping_add(r.addend);
            let (entry_base, bytes) = &mut data_segments[r.seg_idx];
            let off = (r.site - *entry_base) as usize;
            bytes[off..off + 4].copy_from_slice(&value.to_le_bytes());
        }
    }
}

/// Write a 32-bit little-endian integer into a byte buffer at `offset`.
pub(super) fn write_le_i32(buf: &mut [u8], offset: usize, value: i32) {
    buf[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

/// Field-keyed writer over one record instance. Drops the blob borrow
/// between calls so nested-record writers interleave freely.
pub(super) struct RecordWriter<'a> {
    pub layout: &'a RecordLayout,
    pub base: usize,
}
impl<'a> RecordWriter<'a> {
    /// Anchor at an existing record; record bytes must already be in the blob.
    pub(super) fn at(layout: &'a RecordLayout, base: usize) -> Self {
        Self { layout, base }
    }

    /// Append a fresh zeroed record and anchor at it.
    pub(super) fn extend_zero(blob: &mut Vec<u8>, layout: &'a RecordLayout) -> Self {
        let base = blob.len();
        blob.extend(std::iter::repeat_n(0u8, layout.size as usize));
        Self { layout, base }
    }

    /// Absolute byte offset of `field` within the blob.
    pub(super) fn field_offset(&self, field: &str) -> usize {
        self.base + self.layout.offset_of(field) as usize
    }

    pub(super) fn nested<'b>(
        &self,
        field: &str,
        nested_layout: &'b RecordLayout,
    ) -> RecordWriter<'b> {
        RecordWriter::at(nested_layout, self.field_offset(field))
    }

    pub(super) fn write_i32(&self, blob: &mut [u8], field: &str, value: i32) {
        write_le_i32(blob, self.field_offset(field), value);
    }

    pub(super) fn write_u8(&self, blob: &mut [u8], field: &str, value: u8) {
        blob[self.field_offset(field)] = value;
    }

    /// Write a `(ptr, len)` slice pair for a `list<T>` / `string` field.
    pub(super) fn write_slice(&self, blob: &mut [u8], field: &str, slice: BlobSlice) {
        let off = self.field_offset(field);
        write_le_i32(blob, off + SLICE_PTR_OFFSET as usize, slice.off as i32);
        write_le_i32(blob, off + SLICE_LEN_OFFSET as usize, slice.len as i32);
    }

    /// Set the option disc byte to `none`. Caller must `extend_zero`
    /// to zero the payload.
    pub(super) fn write_option_none(&self, blob: &mut [u8], field: &str) {
        self.write_u8(blob, field, OPTION_NONE);
    }

    /// Set the option disc to `some`. Caller fills the payload via a
    /// separate writer at `field_offset(field) + payload_off`.
    pub(super) fn write_option_some(&self, blob: &mut [u8], field: &str) {
        self.write_u8(blob, field, OPTION_SOME);
    }
}

#[cfg(test)]
mod reloc_tests {
    use super::*;

    /// Many segments × many relocs — guards the seg_idx threading.
    #[test]
    fn resolve_writes_each_site_in_owning_segment() {
        const N: u32 = 64;
        const SLOTS_PER_SEG: u32 = 8;
        const SEG_BYTES: u32 = SLOTS_PER_SEG * 4;
        // Leave a 4-byte gap between segments so they don't coalesce
        // and seg_idx == placement order.
        const STRIDE: u32 = SEG_BYTES + 4;

        let mut symbols = SymbolBases::new();
        let mut plan = RelocPlan::new();
        let mut data_segments: Vec<(u32, Vec<u8>)> = Vec::new();
        let mut targets: Vec<SymbolId> = Vec::new();

        for i in 0..N {
            let id = symbols.alloc();
            let base = i * STRIDE;
            symbols.set(id, base);
            data_segments.push((base, vec![0u8; SEG_BYTES as usize]));
            targets.push(id);
        }

        // Each segment patches all 8 slots, each pointing at a
        // different segment's base + a unique addend.
        for (seg_idx, (base, _)) in data_segments.iter().enumerate() {
            let relocs: Vec<Reloc> = (0..SLOTS_PER_SEG)
                .map(|s| Reloc {
                    site: s * 4,
                    target: targets[(seg_idx + s as usize) % N as usize],
                    addend: (seg_idx as i32) * 100 + s as i32,
                })
                .collect();
            plan.record_segment(seg_idx, *base, relocs);
        }

        plan.resolve(&symbols, &mut data_segments);

        for (seg_idx, (_, bytes)) in data_segments.iter().enumerate() {
            for s in 0..SLOTS_PER_SEG as usize {
                let off = s * 4;
                let written = i32::from_le_bytes(bytes[off..off + 4].try_into().unwrap());
                let target_base = ((seg_idx + s) % N as usize) as i32 * STRIDE as i32;
                let addend = (seg_idx as i32) * 100 + s as i32;
                assert_eq!(written, target_base + addend);
            }
        }
    }

    /// Coalesced placements share an entry; each reloc still hits
    /// the right local offset.
    #[test]
    fn resolve_handles_coalesced_segments() {
        let mut symbols = SymbolBases::new();
        let mut plan = RelocPlan::new();

        let id_a = symbols.alloc();
        let id_b = symbols.alloc();
        symbols.set(id_a, 0);
        symbols.set(id_b, 8);

        // Single coalesced entry holds both placements.
        let mut data_segments = vec![(0u32, vec![0u8; 16])];

        // Placement A: bytes 0..8, site at offset 4 → target B (=8).
        plan.record_segment(
            0,
            0,
            vec![Reloc {
                site: 4,
                target: id_b,
                addend: 0,
            }],
        );
        // Placement B: bytes 8..16, site at offset 0 → target A (=0) + 3.
        plan.record_segment(
            0,
            8,
            vec![Reloc {
                site: 0,
                target: id_a,
                addend: 3,
            }],
        );

        plan.resolve(&symbols, &mut data_segments);

        let bytes = &data_segments[0].1;
        assert_eq!(i32::from_le_bytes(bytes[4..8].try_into().unwrap()), 8);
        assert_eq!(i32::from_le_bytes(bytes[8..12].try_into().unwrap()), 3);
    }
}
