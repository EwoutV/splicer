//! Flags-info layout. One entry per `Cell::Flags` appearance.
//! Entries live in a per-(fn, param | result) buffer the wrapper
//! allocates per call via `cabi_realloc`; the emitter writes the
//! build-time-const `type-name` + `set-flags.ptr` and the
//! runtime-bit-walked `set-flags.len` into each slot.
//! `field_tree.flags_infos` is patched at runtime to point at the
//! buffer.
//!
//! `set-flags.ptr` points at a per-cell scratch slab the bit-walker
//! fills with `(name_ptr, name_len)` pairs each call. The slab is
//! build-time-static for non-list-element flags (today); list-element
//! flags will add a per-iteration scratch alongside the indices buffer
//! when `Cell::Flags` is opened in `list_element_class`.
//!
//! Per-call (vs. baked-in-static) is forced by `list<flags>`: the
//! count is len-dependent and `field_tree.flags_infos` must span all
//! entries contiguously.

use super::super::super::super::abi::emit::{BlobSlice, STRING_FLAT_BYTES};
use super::super::super::FuncClassified;
use super::super::classify::ResultSource;
use super::super::plan::{Cell, LiftPlan};
use super::PerCellIndices;

/// Where a `Cell::Flags`'s slot lives in the per-(fn, param | result)
/// flags-info buffer, plus where its set-flags scratch slot is.
/// Static cells use the build-time-const slab address; list-element
/// cells stripe both the entry idx and the scratch ptr off per-iter
/// bases.
#[derive(Clone, Copy, Debug)]
pub(crate) enum FlagsSlotSource {
    /// Outer-plan flags: entry idx is build-time-const; the scratch
    /// slab is reserved at layout time at `scratch_addr`.
    Static { entry_idx: u32, scratch_addr: i32 },
    /// List-element flags: entry idx is `list_elem_flags_base +
    /// entry_offset_in_elem`; scratch is `list.flags_scratch_base +
    /// j * flags_scratch_bytes_per_elem + scratch_offset_in_elem`.
    /// Both `*_offset_in_elem`s are build-time-known (the cell's
    /// position among the element-plan's flags cells); the runtime
    /// locals are staged by the list-of-arm.
    PerIteration {
        entry_offset_in_elem: u32,
        scratch_offset_in_elem: u32,
    },
}

/// Per-(plan-cell) emit-phase data for one `Cell::Flags`. The
/// wrapper writes `type-name` + `set-flags.ptr` + `set-flags.len`
/// (the first two build-time-const for static cells / runtime-staged
/// for list-element cells; `len` always runtime from the bit-walk)
/// into the buffer slot resolved from `slot_source`. Cell payload
/// is `cell::flags-set(idx)`.
#[derive(Clone, Debug)]
pub(crate) struct FlagsRuntimeFill {
    /// Slot location + scratch source. See [`FlagsSlotSource`].
    pub slot_source: FlagsSlotSource,
    /// `(off, len)` of the type-name into the shared name blob.
    pub type_name: BlobSlice,
    /// Each flag's interned `(off, len)`, in bit-position order.
    pub flag_names: Vec<BlobSlice>,
}

/// Output of the per-cell walk over every `Cell::Flags` appearance.
/// Mirrors [`super::handle_info::HandleInfoMaps`] — the buffer is
/// per-call so there's no segment to lay out, just per-(fn, param |
/// result) entry counts + per-cell fills.
pub(crate) struct FlagsInfoMaps {
    /// Per-cell fills for plan walks (param + compound result).
    pub per_cell_fill: PerCellIndices<FlagsRuntimeFill>,
    /// Per-fn fill for a Direct (sync flat) `Cell::Flags` result.
    pub per_result_single_fill: Vec<Option<FlagsRuntimeFill>>,
    /// Number of flags entries per (fn, param). Drives the per-call
    /// buffer size + the static `field_tree.flags_infos.len` bake.
    pub per_param_count: Vec<Vec<u32>>,
    /// Number of flags entries in each fn's result buffer (compound
    /// or single-cell direct).
    pub per_result_count: Vec<u32>,
}

/// Walk every (fn, param | compound-result | direct-flags-result) to
/// collect per-cell `FlagsRuntimeFill`s + per-(fn, param | result)
/// entry counts. Caller supplies `scratch_addrs`, one pre-reserved
/// address per `Cell::Flags` in the order this fn consumes them
/// (matches [`flags_scratch_sizes`]). No segment is built — entries
/// are written into a per-call `cabi_realloc`'d buffer at emit time.
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
/// type-name + flag-names off the cell. List-element flags use
/// `PerIteration` (assigned by [`super::super::emit::walk_element_plan`])
/// and don't participate in this static count.
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

/// Per-`Cell::Flags` scratch byte count, in the same plan-walk order
/// [`build_flags_info_maps`] consumes addresses. Walking these in
/// sync is load-bearing: a divergence crashes the builder's
/// `scratch_addrs.next()` expect.
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
