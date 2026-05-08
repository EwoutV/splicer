//! Handle-info layout. One entry per `Cell::Handle` appearance
//! (own/borrow/stream/future/error-context all share — only the
//! cell-disc differs). Entries live in a per-(fn, param | result)
//! buffer the wrapper allocates per call via `cabi_realloc`; the
//! emitter writes the build-time-const `type-name` and the
//! runtime-zero-extended `id` into each slot.
//! `field_tree.handle_infos` is patched at runtime to point at the
//! buffer.
//!
//! Per-call (vs. baked-in-static) is forced by `list<own<R>>` /
//! `list<error-context>` etc.: the count is len-dependent and
//! `field_tree.handle_infos` must span all entries contiguously.

use super::super::super::super::abi::emit::BlobSlice;
use super::super::super::FuncClassified;
use super::super::classify::ResultSource;
use super::super::plan::{Cell, LiftPlan};
use super::PerCellIndices;

/// Where a `Cell::Handle`'s slot lives in the per-(fn, param | result)
/// handle-info buffer.
#[derive(Clone, Copy, Debug)]
pub(crate) enum HandleSlotSource {
    /// Build-time absolute index — outer-plan handles.
    Static(u32),
    /// List-element handle: idx = `list_elem_handle_base +
    /// offset_in_elem` at runtime (the iter-base local is
    /// `handle_slot_base + j * handles_per_elem`, staged by the
    /// list-of-arm).
    PerIteration { offset_in_elem: u32 },
}

/// Per-cell emit-phase data for one `Cell::Handle`: where in the
/// handle-info buffer it lives, plus its build-time `type-name`
/// blob slice. Cell payload is `cell::*-handle(idx)` with the same
/// idx as `slot_source`.
#[derive(Clone, Debug)]
pub(crate) struct HandleRuntimeFill {
    pub slot_source: HandleSlotSource,
    pub type_name: BlobSlice,
}

pub(crate) struct HandleInfoMaps {
    /// Per-cell fills for plan walks (param + compound result).
    pub per_cell_fill: PerCellIndices<HandleRuntimeFill>,
    /// Per-fn fill for a Direct (sync flat) `Cell::Handle` result.
    /// `Some` exactly when the fn's result classifies as
    /// `Direct(Cell::Handle)`.
    pub per_result_single_fill: Vec<Option<HandleRuntimeFill>>,
    /// Number of handle entries per (fn, param). `0` when the param
    /// has no handle cells. Drives the per-call buffer size.
    pub per_param_count: Vec<Vec<u32>>,
    /// Number of handle entries in each fn's result buffer (compound
    /// or single-cell direct). `0` when no handle cells in the
    /// result lift.
    pub per_result_count: Vec<u32>,
}

/// Walk every (fn, param | compound-result | direct-handle-result)
/// to collect per-cell `HandleRuntimeFill`s and per-(fn, param | result)
/// entry counts. No segment is built — entries are written into a
/// per-call `cabi_realloc`'d buffer at emit time.
pub(crate) fn build_handle_info_maps(per_func: &[FuncClassified]) -> HandleInfoMaps {
    let mut per_param_fill: Vec<Vec<Vec<Option<HandleRuntimeFill>>>> =
        Vec::with_capacity(per_func.len());
    let mut per_param_count: Vec<Vec<u32>> = Vec::with_capacity(per_func.len());
    let mut per_result_fill: Vec<Vec<Option<HandleRuntimeFill>>> =
        Vec::with_capacity(per_func.len());
    let mut per_result_count: Vec<u32> = Vec::with_capacity(per_func.len());
    let mut per_result_single_fill: Vec<Option<HandleRuntimeFill>> =
        Vec::with_capacity(per_func.len());
    for fd in per_func {
        let mut params_fill = Vec::with_capacity(fd.params.len());
        let mut params_count = Vec::with_capacity(fd.params.len());
        for p in &fd.params {
            let (fill, count) = scan_plan(&p.plan);
            params_fill.push(fill);
            params_count.push(count);
        }
        per_param_fill.push(params_fill);
        per_param_count.push(params_count);
        let (result_fill, result_count, single) = match fd.result_lift.as_ref() {
            Some(rl) => match rl.compound() {
                Some(c) => {
                    let (fill, count) = scan_plan(&c.plan);
                    (fill, count, None)
                }
                None => match &rl.source {
                    // A `ResultSource::Direct(Cell::Handle)` is always exactly
                    // one entry: a single flat result lifts into a single cell.
                    // Multi-handle direct returns aren't representable today.
                    ResultSource::Direct(Cell::Handle { type_name, .. }) => (
                        Vec::new(),
                        1,
                        Some(HandleRuntimeFill {
                            slot_source: HandleSlotSource::Static(0),
                            type_name: *type_name,
                        }),
                    ),
                    _ => (Vec::new(), 0, None),
                },
            },
            None => (Vec::new(), 0, None),
        };
        per_result_fill.push(result_fill);
        per_result_count.push(result_count);
        per_result_single_fill.push(single);
    }
    HandleInfoMaps {
        per_cell_fill: PerCellIndices {
            per_param: per_param_fill,
            per_result: per_result_fill,
        },
        per_result_single_fill,
        per_param_count,
        per_result_count,
    }
}

/// Walk one plan's outer cells, allocating range-relative
/// `Static(side_table_idx)` per `Cell::Handle` and pulling
/// `type_name` off the cell. List-element handles use `PerIteration`
/// (assigned by [`super::super::emit::walk_element_plan`]) and don't
/// participate in this static count.
fn scan_plan(plan: &LiftPlan) -> (Vec<Option<HandleRuntimeFill>>, u32) {
    let mut fill_map: Vec<Option<HandleRuntimeFill>> =
        (0..plan.cells.len()).map(|_| None).collect();
    let mut count: u32 = 0;
    for (cell_pos, cell) in plan.cells.iter().enumerate() {
        if let Cell::Handle { type_name, .. } = cell {
            fill_map[cell_pos] = Some(HandleRuntimeFill {
                slot_source: HandleSlotSource::Static(count),
                type_name: *type_name,
            });
            count += 1;
        }
    }
    (fill_map, count)
}
