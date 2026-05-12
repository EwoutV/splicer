//! Side-table population: per-tree info records the cell codegen
//! references by build-time-known indices. One builder per kind
//! under the matching sub-module.

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

/// Per-plan-cell side-table data. One entry per `plan.cells`; `None`
/// for cells that lift purely from flat slots. Heavy payloads Boxed
/// to keep the enum ~16 bytes.
#[derive(Clone, Debug)]
pub(crate) enum CellSideData {
    None,
    Record(Box<RecordRuntimeFill>),
    Tuple { source: TupleIdxSource },
    Flags(Box<FlagsRuntimeFill>),
    Variant(Box<VariantRuntimeFill>),
    Char { scratch: CharScratch },
    Handle(Box<HandleRuntimeFill>),
}

/// Where a `Cell::Char`'s utf-8 scratch buffer lives.
/// `Prestaged`: list element body has staged the addr; no static slab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CharScratch {
    Static { scratch_addr: i32 },
    Prestaged,
}

/// Where a `Cell::TupleOf`'s child-index array lives.
/// `PerIteration`: list element body writes per-call into a
/// `cabi_realloc`'d buffer at `offset_in_elem`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TupleIdxSource {
    Static(BlobSlice),
    PerIteration { offset_in_elem: u32 },
}

/// Per-cell fill maps for one (fn, param | result), each parallel to
/// `plan.cells`. Bundled to keep `fold_cell_side_data`'s signature stable.
pub(crate) struct CellFillSources<'a> {
    pub record_fill: &'a [Option<RecordRuntimeFill>],
    pub tuple_indices: &'a [Option<BlobSlice>],
    pub flags_fill: &'a [Option<FlagsRuntimeFill>],
    pub variant_fill: &'a [Option<VariantRuntimeFill>],
    pub char_scratch: &'a [Option<i32>],
    pub handle_fill: &'a [Option<HandleRuntimeFill>],
}

/// Fold per-builder per-cell maps into one `Vec<CellSideData>`.
/// **Outer plan only** — element-plan side data is produced by
/// `super::emit::walk_element_plan` (Prestaged scratch).
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
            // Flat-only cells; explicit (no `_`) so a new variant
            // forces a fold-arm decision at compile time.
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
/// per-fn `single_fill` overlay. Pass `&mut []` when no Direct path.
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

/// Per-(fn, param) and per-(fn, result) per-plan-cell `Option<T>` map.
pub(crate) struct PerCellIndices<T> {
    pub(super) per_param: Vec<Vec<Vec<Option<T>>>>,
    pub(super) per_result: Vec<Vec<Option<T>>>,
}

impl<T> PerCellIndices<T> {
    pub(crate) fn for_param(&self, fn_idx: usize, param_idx: usize) -> &[Option<T>] {
        &self.per_param[fn_idx][param_idx]
    }

    /// Empty slice for non-compound (or void) results.
    pub(crate) fn for_result(&self, fn_idx: usize) -> &[Option<T>] {
        &self.per_result[fn_idx]
    }
}

impl PerCellIndices<SymRef> {
    /// Resolve one (fn, param)'s symbolic cell slots to absolute slices.
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

// Side-table-info records share `{ type-name, <item>-name }`; the
// item-name field name is hard-coded per kind's builder.
pub(super) const INFO_TYPE_NAME: &str = "type-name";

/// Per-(fn, param | result) side-table blob output. `None` marks
/// "no entries for this slot".
pub(crate) struct SideTableBlob {
    pub segment: Segment,
    pub per_param: Vec<Vec<Option<SymRef>>>,
    pub per_result: Vec<Option<SymRef>>,
}
