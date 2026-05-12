//! Variant-info layout. One entry per `Cell::Variant`. Per-call buffer
//! holds `type-name` + disc-dispatched `case-name` + `payload`.

use super::super::super::super::abi::emit::BlobSlice;
use super::super::super::FuncClassified;
use super::super::plan::{Cell, LiftPlan};
use super::PerCellIndices;

/// Where a `Cell::Variant`'s slot lives.
#[derive(Clone, Copy, Debug)]
pub(crate) enum VariantSlotSource {
    /// Outer-plan: build-time entry_idx.
    Static { entry_idx: u32 },
    /// List-element: idx = `list_elem_variant_base + entry_offset_in_elem`.
    PerIteration { entry_offset_in_elem: u32 },
}

#[derive(Clone, Debug)]
pub(crate) struct VariantRuntimeFill {
    pub slot_source: VariantSlotSource,
    pub type_name: BlobSlice,
    pub case_names: Vec<BlobSlice>,
    /// Per-case child cell idx in disc order; `None` for unit cases.
    pub per_case_payload: Vec<Option<u32>>,
}

pub(crate) struct VariantInfoMaps {
    pub per_cell_fill: PerCellIndices<VariantRuntimeFill>,
    pub per_param_count: Vec<Vec<u32>>,
    /// Variant always retptrs — no Direct path.
    pub per_result_count: Vec<u32>,
}

/// Collect per-cell `VariantRuntimeFill`s + entry counts. Variant
/// always retptrs, so no Direct arm.
pub(crate) fn build_variant_info_maps(per_func: &[FuncClassified]) -> VariantInfoMaps {
    let mut per_param_fill: Vec<Vec<Vec<Option<VariantRuntimeFill>>>> =
        Vec::with_capacity(per_func.len());
    let mut per_param_count: Vec<Vec<u32>> = Vec::with_capacity(per_func.len());
    let mut per_result_fill: Vec<Vec<Option<VariantRuntimeFill>>> =
        Vec::with_capacity(per_func.len());
    let mut per_result_count: Vec<u32> = Vec::with_capacity(per_func.len());
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
        let (result_fill, result_count) = match fd.result_lift.as_ref().and_then(|rl| rl.compound())
        {
            Some(c) => scan_plan(&c.plan),
            None => (Vec::new(), 0),
        };
        per_result_fill.push(result_fill);
        per_result_count.push(result_count);
    }
    VariantInfoMaps {
        per_cell_fill: PerCellIndices {
            per_param: per_param_fill,
            per_result: per_result_fill,
        },
        per_param_count,
        per_result_count,
    }
}

/// Walk outer cells, allocate `Static { entry_idx }` per Variant.
fn scan_plan(plan: &LiftPlan) -> (Vec<Option<VariantRuntimeFill>>, u32) {
    let mut fill_map: Vec<Option<VariantRuntimeFill>> =
        (0..plan.cells.len()).map(|_| None).collect();
    let mut count: u32 = 0;
    for (cell_pos, cell) in plan.cells.iter().enumerate() {
        if let Cell::Variant {
            type_name,
            case_names,
            per_case_payload,
            ..
        } = cell
        {
            fill_map[cell_pos] = Some(VariantRuntimeFill {
                slot_source: VariantSlotSource::Static { entry_idx: count },
                type_name: *type_name,
                case_names: case_names.clone(),
                per_case_payload: per_case_payload.clone(),
            });
            count += 1;
        }
    }
    (fill_map, count)
}
