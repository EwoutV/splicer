//! Flags-info layout. One entry per `Cell::Flags`. Per-call buffer
//! holds `type-name` + `set-flags.ptr` (const) + `set-flags.len`
//! (runtime bit-walk). `set-flags.ptr` points at a per-cell scratch
//! slab (static for outer cells; per-iter for list-element flags).

use super::super::super::super::abi::emit::{BlobSlice, STRING_FLAT_BYTES};
use super::super::super::FuncClassified;
use super::super::classify::ResultSource;
use super::super::plan::{Cell, LiftPlan};
use super::PerCellIndices;

/// Where a `Cell::Flags`'s slot + scratch live.
#[derive(Clone, Copy, Debug)]
pub(crate) enum FlagsSlotSource {
    /// Outer-plan: build-time entry idx + reserved scratch_addr.
    Static { entry_idx: u32, scratch_addr: i32 },
    /// List-element: entry idx = `list_elem_flags_base + entry_offset_in_elem`;
    /// scratch = `flags_scratch_base + j*stride + scratch_offset_in_elem`.
    PerIteration {
        entry_offset_in_elem: u32,
        scratch_offset_in_elem: u32,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct FlagsRuntimeFill {
    pub slot_source: FlagsSlotSource,
    pub type_name: BlobSlice,
    /// Flag names in bit-position order.
    pub flag_names: Vec<BlobSlice>,
}

pub(crate) struct FlagsInfoMaps {
    pub per_cell_fill: PerCellIndices<FlagsRuntimeFill>,
    pub per_result_single_fill: Vec<Option<FlagsRuntimeFill>>,
    pub per_param_count: Vec<Vec<u32>>,
    pub per_result_count: Vec<u32>,
}

/// Collect per-cell `FlagsRuntimeFill`s + entry counts. `scratch_addrs`
/// holds pre-reserved scratch addrs in `flags_scratch_sizes` order.
pub(crate) fn build_flags_info_maps(
    per_func: &[FuncClassified],
    scratch_addrs: &mut impl Iterator<Item = u32>,
) -> FlagsInfoMaps {
    let mut per_param_fill: Vec<Vec<Vec<Option<FlagsRuntimeFill>>>> =
        Vec::with_capacity(per_func.len());
    let mut per_param_count: Vec<Vec<u32>> = Vec::with_capacity(per_func.len());
    let mut per_result_fill: Vec<Vec<Option<FlagsRuntimeFill>>> =
        Vec::with_capacity(per_func.len());
    let mut per_result_count: Vec<u32> = Vec::with_capacity(per_func.len());
    let mut per_result_single_fill: Vec<Option<FlagsRuntimeFill>> =
        Vec::with_capacity(per_func.len());
    for fd in per_func {
        let mut params_fill = Vec::with_capacity(fd.params.len());
        let mut params_count = Vec::with_capacity(fd.params.len());
        for p in &fd.params {
            let (fill, count) = scan_plan(&p.plan, scratch_addrs);
            params_fill.push(fill);
            params_count.push(count);
        }
        per_param_fill.push(params_fill);
        per_param_count.push(params_count);
        let (result_fill, result_count, single) = match fd.result_lift.as_ref() {
            Some(rl) => match rl.compound() {
                Some(c) => {
                    let (fill, count) = scan_plan(&c.plan, scratch_addrs);
                    (fill, count, None)
                }
                None => match &rl.source {
                    // Direct(Flags) is exactly one entry — same single-
                    // entry rule as Direct(Handle).
                    ResultSource::Direct(cell @ Cell::Flags { .. }) => (
                        Vec::new(),
                        1,
                        Some(build_static_fill(0, cell, scratch_addrs)),
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
    FlagsInfoMaps {
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
/// `Static { entry_idx, scratch_addr }` per `Cell::Flags` and pulling
/// Walk outer cells, allocating Static(idx) per `Cell::Flags`.
/// List-element flags are set by walk_element_plan.
fn scan_plan(
    plan: &LiftPlan,
    scratch_addrs: &mut impl Iterator<Item = u32>,
) -> (Vec<Option<FlagsRuntimeFill>>, u32) {
    let mut fill_map: Vec<Option<FlagsRuntimeFill>> = (0..plan.cells.len()).map(|_| None).collect();
    let mut count: u32 = 0;
    for (cell_pos, cell) in plan.cells.iter().enumerate() {
        if matches!(cell, Cell::Flags { .. }) {
            fill_map[cell_pos] = Some(build_static_fill(count, cell, scratch_addrs));
            count += 1;
        }
    }
    (fill_map, count)
}

fn build_static_fill(
    entry_idx: u32,
    cell: &Cell,
    scratch_addrs: &mut impl Iterator<Item = u32>,
) -> FlagsRuntimeFill {
    let Cell::Flags {
        type_name,
        flag_names,
        ..
    } = cell
    else {
        unreachable!("build_static_fill called on non-Flags cell {cell:?}");
    };
    let scratch_addr = scratch_addrs
        .next()
        .expect("layout phase must reserve one scratch slot per Cell::Flags cell");
    FlagsRuntimeFill {
        slot_source: FlagsSlotSource::Static {
            entry_idx,
            scratch_addr: scratch_addr as i32,
        },
        type_name: *type_name,
        flag_names: flag_names.clone(),
    }
}

/// Per-`Cell::Flags` scratch byte counts in the same plan-walk order
/// `build_flags_info_maps` consumes addresses (divergence crashes the
/// builder's `scratch_addrs.next()` expect).
pub(crate) fn flags_scratch_sizes(per_func: &[FuncClassified]) -> Vec<u32> {
    let mut sizes = Vec::new();
    for fd in per_func {
        for p in &fd.params {
            collect_flags_sizes(&p.plan, &mut sizes);
        }
        if let Some(rl) = &fd.result_lift {
            if let Some(c) = rl.compound() {
                collect_flags_sizes(&c.plan, &mut sizes);
            } else if let ResultSource::Direct(Cell::Flags { flag_names, .. }) = &rl.source {
                sizes.push(flag_names.len() as u32 * STRING_FLAT_BYTES);
            }
        }
    }
    sizes
}

fn collect_flags_sizes(plan: &LiftPlan, sizes: &mut Vec<u32>) {
    for cell in &plan.cells {
        if let Cell::Flags { flag_names, .. } = cell {
            sizes.push(flag_names.len() as u32 * STRING_FLAT_BYTES);
        }
    }
}
