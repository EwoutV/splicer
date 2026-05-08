//! Side-table population: per-tree info records that the cell
//! codegen references by adapter-build-time-known indices.
//!
//! All side-table kinds (enum / flags / variant / record) share the
//! same shape and lifecycle:
//!   1. Walk every (fn, param | result); for each lift carrying an
//!      info of this kind, dedup-register the strings (type-name +
//!      item-names) into the shared name_blob.
//!   2. Lay out one entry record per item in declaration order, into
//!      one contiguous side-table data segment.
//!   3. Hand back per-(fn, param) and per-(fn, result) [`SymRef`]
//!      pointers tagged with the segment's [`SymbolId`]; the layout
//!      phase resolves them to absolute [`BlobSlice`]s after every
//!      segment has a base.
//!
//! Each side-table kind has its own builder under the matching
//! sub-module ([`enum_info`], [`flags_info`], etc.). Type-name +
//! item-name [`BlobSlice`]s are interned at plan-build time and
//! live on each `Cell::*` directly — the builders here just walk
//! cells and stitch entries.

use super::super::super::abi::emit::BlobSlice;
use super::super::blob::{resolve, Segment, SymRef, SymbolBases};
use super::plan::{Cell, LiftPlan};

pub(super) mod char_info;
pub(super) mod enum_info;
pub(super) mod flags_info;
pub(super) mod handle_info;
pub(super) mod record_info;
pub(super) mod tuple_indices;
pub(super) mod variant_info;

use flags_info::FlagsRuntimeFill;
use handle_info::HandleRuntimeFill;
use record_info::RecordRuntimeFill;
use variant_info::VariantRuntimeFill;

/// Per-plan-cell side-table data the emit phase reads. One entry per
/// `plan.cells` position; `None` for cells that lift purely from flat
/// slots. Heavy payloads (Flags, eventually Variant) are Boxed so the
/// enum stays ~16 bytes — adding a kind = one variant + one
/// [`super::emit::emit_cell_op`] arm.
#[derive(Clone, Debug)]
pub(crate) enum CellSideData {
    None,
    /// `cell::record-of(u32)` payload + the addresses the wrapper
    /// patches at runtime (type-name + fields slice). Static cells'
    /// fields_ptr is build-time-const (resolved post-layout); list-
    /// element cells stage it per iteration. Boxed for the same
    /// reason as Flags/Variant.
    Record(Box<RecordRuntimeFill>),
    /// `cell::tuple-of(list<u32>)` payload source. See
    /// [`TupleIdxSource`] for which buffer the `(ptr, len)` points at.
    Tuple {
        source: TupleIdxSource,
    },
    /// `cell::flags-set(u32)` payload + the addresses the wrapper
    /// bit-walk patches at runtime.
    Flags(Box<FlagsRuntimeFill>),
    /// `cell::variant-case(u32)` payload + the addresses the wrapper
    /// disc-dispatch patches at runtime (case-name + payload option).
    Variant(Box<VariantRuntimeFill>),
    /// Scratch-buffer source for `Cell::Char`'s utf-8 encoder. The
    /// wrapper writes 1–4 bytes into the buffer and emits
    /// `cell::text(scratch, len)`. See [`CharScratch`] for which kind
    /// of buffer this points at.
    Char {
        scratch: CharScratch,
    },
    /// `cell::{resource,stream,future}-handle(u32)` payload + the
    /// wrapper-patched `id` slot address. The cell's `kind` picks
    /// the disc; the side-table layout is identical across all
    /// three. Boxed for the same reason as Flags/Variant.
    Handle(Box<HandleRuntimeFill>),
}

/// Where a `Cell::Char`'s utf-8 scratch buffer lives. Two cases
/// because the buffer base reaches the encoder differently:
///
/// - `Static`: a 4-byte slab the layout phase reserved for this
///   plan-cell. The emit phase stages the const into the wrapper's
///   shared scratch-addr local before each char-cell write.
/// - `Prestaged`: the cell sits inside a `list<char>` element body;
///   the per-iteration emit code has already computed
///   `list_scratch_base + j*4` into the same shared local before
///   calling [`super::emit::emit_cell_op`]. Static slabs aren't
///   reserved for these cells — list length is runtime-only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CharScratch {
    Static { scratch_addr: i32 },
    Prestaged,
}

/// Where a `Cell::TupleOf`'s child-index array lives.
///
/// - `Static`: indices baked into the per-build `tuple-indices`
///   segment by the layout phase; cell payload reads `(slice.off,
///   slice.len)` as build-time consts.
/// - `PerIteration`: the cell sits inside a `list<tuple<...>>`
///   element body; each iteration writes `[elem_cell_base + child_pos[i]]`
///   into a per-call `cabi_realloc`'d buffer. `offset_in_elem` is
///   the byte offset of this cell's slot within the per-iteration
///   sub-region of the buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TupleIdxSource {
    Static(BlobSlice),
    PerIteration { offset_in_elem: u32 },
}

/// Per-cell fill maps for one (fn, param | result), each parallel to
/// `plan.cells` and sourced from the matching side-table builder.
/// Bundled to keep [`fold_cell_side_data`]'s signature stable as new
/// kinds land — adding one here + the matching `fold_cell_side_data`
/// arm is the full change.
pub(crate) struct CellFillSources<'a> {
    pub record_fill: &'a [Option<RecordRuntimeFill>],
    pub tuple_indices: &'a [Option<BlobSlice>],
    pub flags_fill: &'a [Option<FlagsRuntimeFill>],
    pub variant_fill: &'a [Option<VariantRuntimeFill>],
    pub char_scratch: &'a [Option<i32>],
    pub handle_fill: &'a [Option<HandleRuntimeFill>],
}

/// Fold the per-builder per-cell maps into one [`Vec<CellSideData>`]
/// parallel to `plan.cells`. Single match-on-`Cell` is the only place
/// that decides "this cell wants that kind's bookkeeping."
///
/// **Outer plan only.** Element-plan side data is produced by
/// [`super::emit::elem_cell_side_data`] (Prestaged scratch); recursing
/// here would double-fold list-element chars with stale `Static`
/// addresses. `char_scratch_sizes` / `build_char_scratch_map` follow
/// the same rule.
pub(crate) fn fold_cell_side_data(
    plan: &LiftPlan,
    sources: &CellFillSources<'_>,
) -> Vec<CellSideData> {
    let n = plan.cells.len();
    debug_assert_eq!(sources.record_fill.len(), n);
    debug_assert_eq!(sources.tuple_indices.len(), n);
    debug_assert_eq!(sources.flags_fill.len(), n);
    debug_assert_eq!(sources.variant_fill.len(), n);
    debug_assert_eq!(sources.char_scratch.len(), n);
    debug_assert_eq!(sources.handle_fill.len(), n);
    plan.cells
        .iter()
        .enumerate()
        .map(|(i, cell)| match cell {
            // Side-data-bearing kinds.
            Cell::RecordOf { .. } => CellSideData::Record(Box::new(
                sources.record_fill[i]
                    .clone()
                    .expect("RecordOf cell missing runtime-fill bundle"),
            )),
            Cell::TupleOf { .. } => CellSideData::Tuple {
                source: TupleIdxSource::Static(
                    sources.tuple_indices[i].expect("TupleOf cell missing tuple-indices slice"),
                ),
            },
            Cell::Flags { .. } => CellSideData::Flags(Box::new(
                sources.flags_fill[i]
                    .clone()
                    .expect("Flags cell missing runtime-fill bundle"),
            )),
            Cell::Variant { .. } => CellSideData::Variant(Box::new(
                sources.variant_fill[i]
                    .clone()
                    .expect("Variant cell missing runtime-fill bundle"),
            )),
            Cell::Char { .. } => CellSideData::Char {
                scratch: CharScratch::Static {
                    scratch_addr: sources.char_scratch[i].expect("Char cell missing scratch addr"),
                },
            },
            Cell::Handle { .. } => CellSideData::Handle(Box::new(
                sources.handle_fill[i]
                    .clone()
                    .expect("Handle cell missing runtime-fill bundle"),
            )),
            // Wired primitives + control-flow cells that read purely
            // from flat slots — no side-table contribution. Listed
            // explicitly (no `_` catchall) so adding a new wired
            // variant forces a fold-arm decision at compile time,
            // mirroring [`super::emit::emit_cell_op`].
            Cell::Bool { .. }
            | Cell::IntegerSignExt { .. }
            | Cell::IntegerZeroExt { .. }
            | Cell::Integer64 { .. }
            | Cell::FloatingF32 { .. }
            | Cell::FloatingF64 { .. }
            | Cell::Text { .. }
            | Cell::Bytes { .. }
            | Cell::EnumCase { .. }
            | Cell::Option { .. }
            | Cell::Result { .. }
            | Cell::ListOf { .. } => CellSideData::None,
        })
        .collect()
}

// ─── Generic per-cell back-fill helper ───────────────────────────

/// Apply `patch` to every `Some` fill across the per-cell grid + the
/// per-fn `single_fill` overlay. Pass `&mut []` for `single_fill` for
/// kinds without a Direct path (variant).
pub(super) fn back_fill_per_cell<F>(
    fill: &mut PerCellIndices<F>,
    single_fill: &mut [Option<F>],
    mut patch: impl FnMut(&mut F),
) {
    for fn_row in fill.per_param.iter_mut() {
        for param_row in fn_row.iter_mut() {
            for slot in param_row.iter_mut() {
                if let Some(f) = slot.as_mut() {
                    patch(f);
                }
            }
        }
    }
    for fn_row in fill.per_result.iter_mut() {
        for slot in fn_row.iter_mut() {
            if let Some(f) = slot.as_mut() {
                patch(f);
            }
        }
    }
    for slot in single_fill.iter_mut() {
        if let Some(f) = slot.as_mut() {
            patch(f);
        }
    }
}

// ─── Per-cell side-table indices ─────────────────────────────────
//
// Each builder produces its own `PerCellIndices<T>` (record-info: u32,
// tuple-indices: SymRef, flags: FlagsRuntimeFill, variant:
// VariantRuntimeFill). The layout phase folds these into one
// `Vec<CellSideData>` per (fn, param | result) via
// [`fold_cell_side_data`].

/// Per-(fn, param) and per-(fn, result) per-plan-cell `Option<T>`
/// map. Internal nesting is `Vec<Vec<Vec<…>>>` / `Vec<Vec<…>>` but
/// hidden behind [`Self::for_param`] / [`Self::for_result`].
pub(crate) struct PerCellIndices<T> {
    pub(super) per_param: Vec<Vec<Vec<Option<T>>>>,
    pub(super) per_result: Vec<Vec<Option<T>>>,
}

impl<T> PerCellIndices<T> {
    pub(crate) fn for_param(&self, fn_idx: usize, param_idx: usize) -> &[Option<T>] {
        &self.per_param[fn_idx][param_idx]
    }

    /// Per-cell map for one fn's compound result. Empty slice for
    /// non-compound (or void) results.
    pub(crate) fn for_result(&self, fn_idx: usize) -> &[Option<T>] {
        &self.per_result[fn_idx]
    }
}

impl PerCellIndices<SymRef> {
    /// Resolve one (fn, param)'s symbolic cell slots to absolute
    /// [`BlobSlice`]s. Length matches that param's `plan.cells.len()`.
    pub(crate) fn resolve_param(
        &self,
        fn_idx: usize,
        param_idx: usize,
        symbols: &SymbolBases,
    ) -> Vec<Option<BlobSlice>> {
        resolve_cell_syms(self.for_param(fn_idx, param_idx), symbols)
    }

    pub(crate) fn resolve_result(
        &self,
        fn_idx: usize,
        symbols: &SymbolBases,
    ) -> Vec<Option<BlobSlice>> {
        resolve_cell_syms(self.for_result(fn_idx), symbols)
    }
}

fn resolve_cell_syms(syms: &[Option<SymRef>], symbols: &SymbolBases) -> Vec<Option<BlobSlice>> {
    syms.iter()
        .map(|s| s.map(|s| resolve(Some(s), symbols)))
        .collect()
}

// ─── WIT names referenced by lift codegen ─────────────────────────
//
// Side-table-info records in `splicer:common/types` share the same
// shape: `record { type-name: string, <item>-name: string }`. The
// per-kind item-name field name (e.g. `"case-name"` for enum-info,
// `"flag-name"` for flags-info) is hard-coded in each kind's blob
// builder.
pub(super) const INFO_TYPE_NAME: &str = "type-name";

/// Output of the per-(fn, param | result) side-table blob builders.
/// `None` marks "no entries for this slot" — params/results that
/// don't carry this side-table kind. Resolution to absolute
/// [`BlobSlice`]s happens once the segment's base is known.
pub(crate) struct SideTableBlob {
    pub segment: Segment,
    pub per_param: Vec<Vec<Option<SymRef>>>,
    pub per_result: Vec<Option<SymRef>>,
}
