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
use super::super::super::blob::NameInterner;
use super::super::super::FuncClassified;
use super::super::plan::{Cell, LiftPlan, NamedListInfo};
use super::{register_side_table_strings, PerCellIndices, StringTable};

/// Where a `Cell::Flags`'s slot lives in the per-(fn, param | result)
/// flags-info buffer.
#[derive(Clone, Copy, Debug)]
pub(crate) enum FlagsSlotSource {
    /// Build-time absolute index — outer-plan flags.
    Static(u32),
    // List-element variant lands when `Cell::Flags` is opened in
    // `Cell::list_element_class`. Will mirror `HandleSlotSource::PerIteration`:
    // idx = `list_elem_flags_base + offset_in_elem` at runtime.
}

/// Per-(plan-cell) emit-phase data for one `Cell::Flags`. The
/// wrapper writes `type-name` + `set-flags.ptr` (build-time-const)
/// and `set-flags.len` (runtime) into the buffer slot resolved
/// from `slot_source`; cell payload is `cell::flags-set(idx)`.
#[derive(Clone, Debug)]
pub(crate) struct FlagsRuntimeFill {
    /// Slot index into the (fn, param | result) flags-info buffer.
    pub slot_source: FlagsSlotSource,
    /// `(off, len)` of the type-name into the shared name blob.
    pub type_name: BlobSlice,
    /// Address of the per-cell scratch slab the bit-walker writes
    /// `(name_ptr, name_len)` pairs into. Build-time-static for
    /// non-list-element cells; reserved by the layout phase via
    /// [`flags_scratch_sizes`].
    pub scratch_addr: i32,
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

/// Intern type-name + flag-names for every `Cell::Flags` across all
/// param plans, compound result plans, and single-cell flags results.
pub(crate) fn register_flags_strings(
    per_func: &[FuncClassified],
    names: &mut NameInterner,
) -> StringTable {
    register_side_table_strings(
        per_func,
        names,
        |plan, visit| plan.flags_infos().for_each(visit),
        |st| st.flags_info.as_ref(),
    )
}

/// Walk every (fn, param | compound-result | direct-flags-result) to
/// collect per-cell `FlagsRuntimeFill`s + per-(fn, param | result)
/// entry counts. Caller supplies `scratch_addrs`, one pre-reserved
/// address per `Cell::Flags` in the order this fn consumes them
/// (matches [`flags_scratch_sizes`]). No segment is built — entries
/// are written into a per-call `cabi_realloc`'d buffer at emit time.
pub(crate) fn build_flags_info_maps(
    per_func: &[FuncClassified],
    strings: &StringTable,
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
            let (fill, count) = scan_plan(&p.plan, strings, scratch_addrs);
            params_fill.push(fill);
            params_count.push(count);
        }
        per_param_fill.push(params_fill);
        per_param_count.push(params_count);
        let (result_fill, result_count, single) = match fd.result_lift.as_ref() {
            Some(rl) => match rl.compound() {
                Some(c) => {
                    let (fill, count) = scan_plan(&c.plan, strings, scratch_addrs);
                    (fill, count, None)
                }
                None => match rl.side_table.flags_info.as_ref() {
                    // Direct(Flags) is exactly one entry — same single-
                    // entry rule as Direct(Handle).
                    Some(info) => (
                        Vec::new(),
                        1,
                        Some(build_fill(
                            FlagsSlotSource::Static(0),
                            info,
                            strings,
                            scratch_addrs,
                        )),
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

/// Walk one plan, allocating range-relative `Static(side_table_idx)`
/// per `Cell::Flags` and pulling type-name + flag-names off the cell.
fn scan_plan(
    plan: &LiftPlan,
    strings: &StringTable,
    scratch_addrs: &mut impl Iterator<Item = u32>,
) -> (Vec<Option<FlagsRuntimeFill>>, u32) {
    let mut fill_map: Vec<Option<FlagsRuntimeFill>> =
        (0..plan.cells.len()).map(|_| None).collect();
    let mut count: u32 = 0;
    for (cell_pos, cell) in plan.cells.iter().enumerate() {
        if let Cell::Flags { info, .. } = cell {
            fill_map[cell_pos] = Some(build_fill(
                FlagsSlotSource::Static(count),
                info,
                strings,
                scratch_addrs,
            ));
            count += 1;
        }
    }
    (fill_map, count)
}

fn build_fill(
    slot_source: FlagsSlotSource,
    info: &NamedListInfo,
    strings: &StringTable,
    scratch_addrs: &mut impl Iterator<Item = u32>,
) -> FlagsRuntimeFill {
    let s = strings
        .get(&info.type_name)
        .expect("register_flags_strings ran for every info");
    let scratch_addr = scratch_addrs
        .next()
        .expect("layout phase must reserve one scratch slot per Cell::Flags cell");
    debug_assert_eq!(info.item_names.len(), s.items.len());
    FlagsRuntimeFill {
        slot_source,
        type_name: s.type_name,
        scratch_addr: scratch_addr as i32,
        flag_names: s.items.clone(),
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
            } else if let Some(info) = &rl.side_table.flags_info {
                sizes.push(info.item_names.len() as u32 * STRING_FLAT_BYTES);
            }
        }
    }
    sizes
}

fn collect_flags_sizes(plan: &LiftPlan, sizes: &mut Vec<u32>) {
    for cell in &plan.cells {
        if let Cell::Flags { info, .. } = cell {
            sizes.push(info.item_names.len() as u32 * STRING_FLAT_BYTES);
        }
    }
}
