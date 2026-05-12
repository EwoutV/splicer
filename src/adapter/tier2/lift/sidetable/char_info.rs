//! Char per-cell scratch sizing + addr map (4 bytes for utf-8). No
//! segment of its own — layout reserves slabs, this maps cells to
//! bases. `char_scratch_sizes` and `build_char_scratch_map` must walk
//! in lockstep (divergence crashes `scratch_addrs.next()`).
//! Outer-plan only — list-element chars use per-call scratch.

use super::super::super::super::abi::emit::MAX_UTF8_LEN;
use super::super::super::FuncClassified;
use super::super::classify::ResultSource;
use super::super::plan::{Cell, LiftPlan};
use super::PerCellIndices;

/// Whether `fd`'s result is `Direct(Cell::Char)`. Retptr-routed char
/// results ride Compound and register via the plan walk.
fn result_is_direct_char(fd: &FuncClassified) -> bool {
    let Some(rl) = &fd.result_lift else {
        return false;
    };
    matches!(rl.source, ResultSource::Direct(Cell::Char { .. }))
}

pub(crate) struct CharScratchMaps {
    pub per_cell: PerCellIndices<i32>,
    /// `Some` iff the fn's result is `Direct(Cell::Char)`.
    pub per_result_single: Vec<Option<i32>>,
}

/// Per-`Cell::Char` scratch byte counts in `build_char_scratch_map` order.
pub(crate) fn char_scratch_sizes(per_func: &[FuncClassified]) -> Vec<u32> {
    let mut sizes = Vec::new();
    for fd in per_func {
        for p in &fd.params {
            collect(&p.plan, &mut sizes);
        }
        if let Some(rl) = &fd.result_lift {
            if let Some(c) = rl.compound() {
                collect(&c.plan, &mut sizes);
            } else if result_is_direct_char(fd) {
                sizes.push(MAX_UTF8_LEN);
            }
        }
    }
    sizes
}

fn collect(plan: &LiftPlan, sizes: &mut Vec<u32>) {
    for cell in &plan.cells {
        if matches!(cell, Cell::Char { .. }) {
            sizes.push(MAX_UTF8_LEN);
        }
    }
}

/// Per-`Cell::Char` scratch base map. `scratch_addrs` order matches
/// `char_scratch_sizes`.
pub(crate) fn build_char_scratch_map(
    per_func: &[FuncClassified],
    scratch_addrs: &mut impl Iterator<Item = u32>,
) -> CharScratchMaps {
    let mut per_param: Vec<Vec<Vec<Option<i32>>>> = Vec::with_capacity(per_func.len());
    let mut per_result: Vec<Vec<Option<i32>>> = Vec::with_capacity(per_func.len());
    let mut per_result_single: Vec<Option<i32>> = Vec::with_capacity(per_func.len());
    for fd in per_func {
        let mut params = Vec::with_capacity(fd.params.len());
        for p in &fd.params {
            params.push(map_plan(&p.plan, scratch_addrs));
        }
        per_param.push(params);
        let (compound_map, single) = match fd.result_lift.as_ref() {
            Some(rl) if rl.compound().is_some() => {
                let c = rl.compound().expect("matched Some above");
                (map_plan(&c.plan, scratch_addrs), None)
            }
            _ if result_is_direct_char(fd) => (
                Vec::new(),
                Some(
                    scratch_addrs
                        .next()
                        .expect("layout phase must reserve one scratch slot per char result")
                        as i32,
                ),
            ),
            _ => (Vec::new(), None),
        };
        per_result.push(compound_map);
        per_result_single.push(single);
    }
    CharScratchMaps {
        per_cell: PerCellIndices {
            per_param,
            per_result,
        },
        per_result_single,
    }
}

fn map_plan(plan: &LiftPlan, scratch_addrs: &mut impl Iterator<Item = u32>) -> Vec<Option<i32>> {
    plan.cells
        .iter()
        .map(|cell| match cell {
            Cell::Char { .. } => Some(
                scratch_addrs
                    .next()
                    .expect("layout phase must reserve one scratch slot per Cell::Char")
                    as i32,
            ),
            _ => None,
        })
        .collect()
}
