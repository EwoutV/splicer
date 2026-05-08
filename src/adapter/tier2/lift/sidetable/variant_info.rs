//! Variant-info layout. One entry per `Cell::Variant` appearance.
//! Entries live in a per-(fn, param | result) buffer the wrapper
//! allocates per call via `cabi_realloc`; the emitter writes the
//! build-time-const `type-name` and the runtime-dispatched `case-name`
//! + `payload` (option<u32>) into each slot.
//! `field_tree.variant_infos` is patched at runtime to point at the
//! buffer.
//!
//! Per-call (vs. baked-in-static) is forced by `list<variant>`: the
//! count is len-dependent and `field_tree.variant_infos` must span
//! all entries contiguously.

use super::super::super::super::abi::emit::BlobSlice;
use super::super::super::FuncClassified;
use super::super::plan::{Cell, LiftPlan};
use super::PerCellIndices;

/// Where a `Cell::Variant`'s slot lives in the per-(fn, param | result)
/// variant-info buffer.
#[derive(Clone, Copy, Debug)]
pub(crate) enum VariantSlotSource {
    /// Build-time absolute index — outer-plan variants.
    Static { entry_idx: u32 },
    /// List-element variants: entry idx is `list_elem_variant_base +
    /// entry_offset_in_elem` at runtime. `entry_offset_in_elem` is
    /// build-time-known (the cell's position among the element-plan's
    /// variant cells); the runtime local is staged by the list-of-arm.
    PerIteration { entry_offset_in_elem: u32 },
}

/// Per-(plan-cell) emit-phase data for one `Cell::Variant`. The
/// wrapper writes `type-name` (build-time-const) and the
/// disc-dispatched `case-name` + `payload` into the buffer slot
/// resolved from `slot_source`. Cell payload is `cell::variant-case(idx)`
/// with the same idx as `slot_source`.
#[derive(Clone, Debug)]
pub(crate) struct VariantRuntimeFill {
    pub slot_source: VariantSlotSource,
    /// `(off, len)` of the type-name into the shared name blob.
    pub type_name: BlobSlice,
    /// Pre-interned `(off, len)` of each case-name, in disc order.
    pub case_names: Vec<BlobSlice>,
    /// Per-case child cell idx, in disc order. `None` for unit cases.
    pub per_case_payload: Vec<Option<u32>>,
}

/// Output of the per-cell walk over every `Cell::Variant` appearance.
/// Mirrors [`super::flags_info::FlagsInfoMaps`] — the buffer is
/// per-call so there's no segment to lay out, just per-(fn, param |
/// result) entry counts + per-cell fills.
pub(crate) struct VariantInfoMaps {
    /// Per-cell fills for plan walks (param + compound result).
    pub per_cell_fill: PerCellIndices<VariantRuntimeFill>,
    /// Number of variant entries per (fn, param). Drives the per-call
    /// buffer size + the static `field_tree.variant_infos.len` bake.
    pub per_param_count: Vec<Vec<u32>>,
    /// Number of variant entries in each fn's compound result buffer.
    /// `0` when no variant cells in the result lift; never any Direct
    /// (sync flat) variant results — `Variant` always retptrs.
    pub per_result_count: Vec<u32>,
}

/// Walk every (fn, param | compound-result) to collect per-cell
/// `VariantRuntimeFill`s + per-(fn, param | result) entry counts.
/// No segment is built — entries are written into a per-call
/// `cabi_realloc`'d buffer at emit time.
///
/// `Variant` always retptrs so there's no `Direct` arm; all
/// variant-bearing results route through the compound plan.
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

/// Walk one plan's outer cells, allocating range-relative
/// `Static { entry_idx }` per `Cell::Variant` and pulling type_name +
/// case_names + per_case_payload off the cell. List-element variants
/// (commit-2) will use `PerIteration` and won't participate in this
/// static count.
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
