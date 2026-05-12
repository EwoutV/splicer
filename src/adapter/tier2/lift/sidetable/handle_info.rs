//! Handle-info layout. One entry per `Cell::Handle` (own / borrow /
//! stream / future / error-context). Per-call buffer (vs. static)
//! because `list<own<R>>` etc. make the count len-dependent.

use super::super::super::super::abi::emit::BlobSlice;
use super::super::super::FuncClassified;
use super::super::classify::ResultSource;
use super::super::plan::{Cell, LiftPlan};
use super::PerCellIndices;

/// Where a `Cell::Handle`'s slot lives in the per-call buffer.
#[derive(Clone, Copy, Debug)]
pub(crate) enum HandleSlotSource {
    /// Build-time absolute index — outer-plan handles.
    Static(u32),
    /// List-element: idx = `list_elem_handle_base + offset_in_elem`.
    PerIteration { offset_in_elem: u32 },
}

#[derive(Clone, Debug)]
pub(crate) struct HandleRuntimeFill {
    pub slot_source: HandleSlotSource,
    pub type_name: BlobSlice,
}

pub(crate) struct HandleInfoMaps {
    pub per_cell_fill: PerCellIndices<HandleRuntimeFill>,
    /// `Some` iff the fn's result is `Direct(Cell::Handle)`.
    pub per_result_single_fill: Vec<Option<HandleRuntimeFill>>,
    /// Static (outer-plan) handle entries per (fn, param). Drives
    /// per-call buffer size.
    pub per_param_count: Vec<Vec<u32>>,
    pub per_result_count: Vec<u32>,
}

/// Collect per-cell `HandleRuntimeFill`s + per-(fn, param | result)
/// entry counts. No segment built — entries written per-call.
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
                    // Direct(Handle) is always exactly one entry.
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

/// Walk one plan's outer cells, allocating `Static(idx)` per Handle.
/// List-element handles use `PerIteration` (set by walk_element_plan).
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
