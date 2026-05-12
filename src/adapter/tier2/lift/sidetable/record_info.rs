//! Record-info layout. One entry per `Cell::RecordOf`. Per-call buffer
//! holds `type-name` + `fields.ptr/len`. Outer-plan field-tuples are
//! baked in a static segment; list-element records get per-iter tuples.

use super::super::super::super::abi::emit::{BlobSlice, RecordLayout};
use super::super::super::blob::{RecordWriter, Segment, SymbolId};
use super::super::super::schema::{RECORD_FIELD_TUPLE_IDX, RECORD_FIELD_TUPLE_NAME};
use super::super::super::FuncClassified;
use super::super::plan::{Cell, LiftPlan};
use super::PerCellIndices;

/// Where a `Cell::RecordOf`'s slot + field-tuples sub-slice live.
#[derive(Clone, Copy, Debug)]
pub(crate) enum RecordSlotSource {
    /// Outer-plan: build-time entry_idx + absolute fields_ptr
    /// (segment-relative until `back_fill_record_fields_ptrs`).
    Static { entry_idx: u32, fields_ptr: i32 },
    /// List-element: idx = `list_elem_record_base + entry_offset_in_elem`;
    /// tuples = `list_elem_record_tuples_base + tuples_offset_in_elem`.
    PerIteration {
        entry_offset_in_elem: u32,
        tuples_offset_in_elem: u32,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct RecordRuntimeFill {
    pub slot_source: RecordSlotSource,
    pub type_name: BlobSlice,
    pub fields_len: u32,
}

pub(crate) struct RecordInfoMaps {
    /// Tuples segment (stays baked for outer-plan records).
    pub tuples: Segment,
    pub per_cell_fill: PerCellIndices<RecordRuntimeFill>,
    pub per_param_count: Vec<Vec<u32>>,
    /// `RecordOf` always retptrs — no Direct path.
    pub per_result_count: Vec<u32>,
}

/// Collect per-cell `RecordRuntimeFill`s + counts + the baked tuples
/// segment. RecordOf always retptrs, so no Direct arm.
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

/// Walk outer cells, allocate `Static { entry_idx }` per RecordOf,
/// append field tuples, record segment-relative `fields_ptr`.
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

/// Rebase every `Static` `fields_ptr` from segment-relative to absolute.
pub(crate) fn back_fill_record_fields_ptrs(
    per_cell_fill: &mut PerCellIndices<RecordRuntimeFill>,
    tuples_base: u32,
) {
    super::back_fill_per_cell(per_cell_fill, &mut [], |fill| match &mut fill.slot_source {
        RecordSlotSource::Static { fields_ptr, .. } => {
            *fields_ptr = (tuples_base as i32)
                .checked_add(*fields_ptr)
                .expect("absolute fields_ptr overflowed i32 (>2 GB)");
        }
        // PerIteration computes fields_ptr per call.
        RecordSlotSource::PerIteration { .. } => {}
    });
}
