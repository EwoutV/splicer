//! Record-info layout. One entry per `Cell::RecordOf` appearance.
//! Entries live in a per-(fn, param | result) buffer the wrapper
//! allocates per call via `cabi_realloc`; the emitter writes the
//! build-time-const `type-name` + `fields.ptr` + `fields.len` into
//! each slot. `field_tree.record_infos` is patched at runtime to
//! point at the buffer.
//!
//! The `(field-name, child-cell-idx)` tuples each entry's `fields`
//! slice points at stay baked in a static segment for outer-plan
//! records (their `child-cell-idx` values are build-time-const).
//! List-element records (when `Cell::RecordOf` opens in
//! `list_element_class`) will get a per-iteration tuples sub-slice
//! out of a `cabi_realloc`'d buffer alongside the entries.
//!
//! Per-call (vs. baked-in-static) is forced by `list<record>`: the
//! count is len-dependent and `field_tree.record_infos` must span
//! all entries contiguously.

use super::super::super::super::abi::emit::{BlobSlice, RecordLayout};
use super::super::super::blob::{RecordWriter, Segment, SymbolId};
use super::super::super::schema::{RECORD_FIELD_TUPLE_IDX, RECORD_FIELD_TUPLE_NAME};
use super::super::super::FuncClassified;
use super::super::plan::{Cell, LiftPlan};
use super::PerCellIndices;

/// Where a `Cell::RecordOf`'s slot lives in the per-(fn, param | result)
/// record-info buffer plus where its field-tuples sub-slice lives.
/// Static cells carry a build-time `entry_idx` and a pre-resolved
/// absolute `fields_ptr`; list-element cells stripe both the entry
/// idx and the tuples sub-slice off per-iter bases. Mirrors flags's
/// `Static { entry_idx, scratch_addr } / PerIteration { … }` pair —
/// each variant carries exactly the data the corresponding emit
/// branch consumes, no dead fields.
#[derive(Clone, Copy, Debug)]
pub(crate) enum RecordSlotSource {
    /// Outer-plan records: `entry_idx` is build-time-const and
    /// `fields_ptr` is a pre-resolved absolute address (post
    /// [`back_fill_record_fields_ptrs`]).
    Static { entry_idx: u32, fields_ptr: i32 },
    /// List-element records: entry idx is `list_elem_record_base +
    /// entry_offset_in_elem`; field-tuples sub-slice base is
    /// `list_elem_record_tuples_base + tuples_offset_in_elem`. Both
    /// `*_offset_in_elem`s are build-time-known (the cell's position
    /// among the element-plan's record cells); the runtime locals
    /// are staged by the list-of-arm.
    PerIteration {
        entry_offset_in_elem: u32,
        tuples_offset_in_elem: u32,
    },
}

/// Per-(plan-cell) emit-phase data for one `Cell::RecordOf`. The
/// wrapper writes `type-name` + `fields.ptr` + `fields.len` into the
/// buffer slot resolved from `slot_source`; cell payload is
/// `cell::record-of(idx)` with the same idx as `slot_source`.
#[derive(Clone, Debug)]
pub(crate) struct RecordRuntimeFill {
    pub slot_source: RecordSlotSource,
    /// `(off, len)` of the type-name into the shared name blob.
    pub type_name: BlobSlice,
    /// Number of `(name, idx)` tuples for this record. Build-time-
    /// const for both Static and PerIteration regimes.
    pub fields_len: u32,
}

/// Output of [`build_record_info_maps`]. The tuples segment carries
/// one record's `(field-name, child-cell-idx)` tuples per entry,
/// laid out back-to-back in plan-walk order. The fills' `fields_ptr`
/// values are still segment-relative offsets; layout-phase calls
/// [`back_fill_record_fields_ptrs`] after the segment is placed.
pub(crate) struct RecordInfoMaps {
    /// Tuples segment — stays baked for outer-plan records (their
    /// `child-cell-idx` values are build-time-const). List-element
    /// records will get a separate per-call tuples buffer.
    pub tuples: Segment,
    /// Per-cell fills for plan walks (param + compound result).
    pub per_cell_fill: PerCellIndices<RecordRuntimeFill>,
    /// Number of record entries per (fn, param). Drives the per-call
    /// buffer size + the static `field_tree.record_infos.len` bake.
    pub per_param_count: Vec<Vec<u32>>,
    /// Number of record entries in each fn's compound result buffer.
    /// `0` when no record cells in the result lift; never any Direct
    /// (sync flat) record results — `RecordOf` always retptrs.
    pub per_result_count: Vec<u32>,
}

/// Walk every (fn, param | compound-result) to collect per-cell
/// `RecordRuntimeFill`s + per-(fn, param | result) entry counts +
/// the baked tuples segment. No entries segment is built — entries
/// are written into a per-call `cabi_realloc`'d buffer at emit time.
///
/// `RecordOf` always retptrs so there's no `Direct` arm; all
/// record-bearing results route through the compound plan.
pub(crate) fn build_record_info_maps(
    per_func: &[FuncClassified],
    tuple_layout: &RecordLayout,
    tuples_id: SymbolId,
) -> RecordInfoMaps {
    let mut tuples_bytes: Vec<u8> = Vec::new();
    let mut per_param_fill: Vec<Vec<Vec<Option<RecordRuntimeFill>>>> =
        Vec::with_capacity(per_func.len());
    let mut per_param_count: Vec<Vec<u32>> = Vec::with_capacity(per_func.len());
    let mut per_result_fill: Vec<Vec<Option<RecordRuntimeFill>>> =
        Vec::with_capacity(per_func.len());
    let mut per_result_count: Vec<u32> = Vec::with_capacity(per_func.len());
    for fd in per_func {
        let mut params_fill = Vec::with_capacity(fd.params.len());
        let mut params_count = Vec::with_capacity(fd.params.len());
        for p in &fd.params {
            let (fill, count) = scan_plan(&p.plan, tuple_layout, &mut tuples_bytes);
            params_fill.push(fill);
            params_count.push(count);
        }
        per_param_fill.push(params_fill);
        per_param_count.push(params_count);
        let (result_fill, result_count) = match fd.result_lift.as_ref().and_then(|rl| rl.compound())
        {
            Some(c) => scan_plan(&c.plan, tuple_layout, &mut tuples_bytes),
            None => (Vec::new(), 0),
        };
        per_result_fill.push(result_fill);
        per_result_count.push(result_count);
    }
    RecordInfoMaps {
        tuples: Segment {
            id: tuples_id,
            align: tuple_layout.align,
            bytes: tuples_bytes,
            relocs: Vec::new(),
        },
        per_cell_fill: PerCellIndices {
            per_param: per_param_fill,
            per_result: per_result_fill,
        },
        per_param_count,
        per_result_count,
    }
}

/// Walk one plan's outer cells, allocating range-relative
/// `Static { entry_idx }` per `Cell::RecordOf`, appending its field
/// tuples to `tuples_bytes`, and recording the segment-relative
/// `fields_ptr` offset. List-element records (commit-2) will use
/// `PerIteration` and won't participate in this static count.
fn scan_plan(
    plan: &LiftPlan,
    tuple_layout: &RecordLayout,
    tuples_bytes: &mut Vec<u8>,
) -> (Vec<Option<RecordRuntimeFill>>, u32) {
    let mut fill_map: Vec<Option<RecordRuntimeFill>> =
        (0..plan.cells.len()).map(|_| None).collect();
    let mut count: u32 = 0;
    for (cell_pos, cell) in plan.cells.iter().enumerate() {
        if let Cell::RecordOf { type_name, fields } = cell {
            let tuples_off = tuples_bytes.len() as u32;
            let fields_len = fields.len() as u32;
            for (field_name, child_cell_idx) in fields {
                let tuple = RecordWriter::extend_zero(tuples_bytes, tuple_layout);
                tuple.write_slice(tuples_bytes, RECORD_FIELD_TUPLE_NAME, *field_name);
                tuple.write_i32(tuples_bytes, RECORD_FIELD_TUPLE_IDX, *child_cell_idx as i32);
            }
            fill_map[cell_pos] = Some(RecordRuntimeFill {
                slot_source: RecordSlotSource::Static {
                    entry_idx: count,
                    // Segment-relative until back_fill_record_fields_ptrs runs.
                    fields_ptr: tuples_off as i32,
                },
                type_name: *type_name,
                fields_len,
            });
            count += 1;
        }
    }
    (fill_map, count)
}

/// Rebase every `Static` fill's `fields_ptr` from segment-relative
/// to absolute by adding the placed tuples-segment base. Mirrors
/// the pattern used by [`super::variant_info::back_fill_entry_addrs`]:
/// builder writes offsets, layout-phase patches in place once the
/// segment has a base.
pub(crate) fn back_fill_record_fields_ptrs(
    per_cell_fill: &mut PerCellIndices<RecordRuntimeFill>,
    tuples_base: u32,
) {
    super::back_fill_per_cell(per_cell_fill, &mut [], |fill| match &mut fill.slot_source {
        RecordSlotSource::Static { fields_ptr, .. } => {
            *fields_ptr = (tuples_base as i32)
                .checked_add(*fields_ptr)
                .expect("absolute fields_ptr overflowed i32 — tuples_base + offset > 2 GB");
        }
        // PerIteration `fields_ptr` is computed per call from
        // `list_elem_record_tuples_base + tuples_offset_in_elem` —
        // not rebased off the static tuples segment.
        RecordSlotSource::PerIteration { .. } => {}
    });
}
