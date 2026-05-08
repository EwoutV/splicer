//! Enum-info side-table builder. Reads pre-interned `(type_name,
//! case_names)` BlobSlices off [`Cell::EnumCase`] (interned at
//! plan-build time, mirroring [`Cell::Flags`] and [`Cell::Handle`])
//! and writes one entry per case into the `enum-info` segment. The
//! cell at runtime points at the contiguous per-(param|result) range
//! via `(blob_off, len)`.

use super::super::super::super::abi::emit::RecordLayout;
use super::super::super::blob::{RecordWriter, Segment, SymRef, SymbolId};
use super::super::super::FuncClassified;
use super::super::classify::ResultSource;
use super::super::plan::{Cell, LiftPlan};
use super::{SideTableBlob, INFO_TYPE_NAME};

const ENUM_INFO_CASE_NAME: &str = "case-name";

/// Build the enum-info side-table blob. The selected `Cell::EnumCase`
/// contributes `len(case_names)` entries; the cell's runtime
/// `(off, len)` slice points at the per-(fn, param | result) range.
///
/// **Plans must have ≤1 `EnumCase` per param/result.** Multi-enum
/// plans (e.g. a record with two distinct enum fields) need a
/// per-cell side-table-idx scheme like flags/variant — the cell
/// payload `cell::enum-case(disc)` is the case index, not a
/// per-enum offset, so two enum cells would step on each other in
/// the same per-param range. The plan-builder doesn't enforce this
/// today; the [`debug_assert`] below catches a violation at build
/// time so a future plan-builder change fails loudly instead of
/// silently dropping entries.
pub(crate) fn build_enum_info_blob(
    per_func: &[FuncClassified],
    entry_layout: &RecordLayout,
    segment_id: SymbolId,
) -> SideTableBlob {
    let mut bytes: Vec<u8> = Vec::new();
    let mut per_param: Vec<Vec<Option<SymRef>>> = Vec::with_capacity(per_func.len());
    let mut per_result: Vec<Option<SymRef>> = Vec::with_capacity(per_func.len());
    for fd in per_func {
        let mut params = Vec::with_capacity(fd.params.len());
        for p in &fd.params {
            params.push(append_entries(
                &mut bytes,
                entry_layout,
                segment_id,
                sole_enum_case(&p.plan),
            ));
        }
        per_param.push(params);
        // Compound results: walk the plan (catches enums in list
        // element plans). Direct results: the cell IS the result.
        let result_cell = fd.result_lift.as_ref().and_then(|r| match r.compound() {
            Some(c) => sole_enum_case(&c.plan),
            None => match &r.source {
                ResultSource::Direct(cell @ Cell::EnumCase { .. }) => Some(cell),
                _ => None,
            },
        });
        per_result.push(append_entries(
            &mut bytes,
            entry_layout,
            segment_id,
            result_cell,
        ));
    }
    SideTableBlob {
        segment: Segment {
            id: segment_id,
            align: entry_layout.align,
            bytes,
            relocs: Vec::new(),
        },
        per_param,
        per_result,
    }
}

/// The plan's sole `Cell::EnumCase` (or `None` if the plan has no
/// enums). Walks into list-element sub-plans — `Cell::ListOf` is the
/// only cell kind that carries a sub-`LiftPlan` today; record/tuple/
/// option/result/variant reference children by cell index inside
/// the same plan, so a non-recursive `cells.iter()` would miss
/// `list<enum>`. `debug_assert`s the ≤1 invariant the side-table
/// shape requires.
fn sole_enum_case(plan: &LiftPlan) -> Option<&Cell> {
    let mut found: Option<&Cell> = None;
    visit_enum_cases(plan, &mut |cell| {
        debug_assert!(
            found.is_none(),
            "plan has multiple Cell::EnumCase entries — enum side-table \
             keyed by per-cell disc only supports ≤1 enum per (fn, param | \
             result); see build_enum_info_blob doc",
        );
        found = Some(cell);
    });
    found
}

fn visit_enum_cases<'a>(plan: &'a LiftPlan, visit: &mut impl FnMut(&'a Cell)) {
    for cell in &plan.cells {
        if matches!(cell, Cell::EnumCase { .. }) {
            visit(cell);
        }
        if let Cell::ListOf { element_plan, .. } = cell {
            visit_enum_cases(element_plan, visit);
        }
    }
}

fn append_entries(
    blob: &mut Vec<u8>,
    entry_layout: &RecordLayout,
    segment_id: SymbolId,
    cell: Option<&Cell>,
) -> Option<SymRef> {
    let Cell::EnumCase {
        type_name,
        case_names,
        ..
    } = cell?
    else {
        unreachable!("sole_enum_case returned non-EnumCase");
    };
    let blob_off = blob.len() as u32;
    let len = case_names.len() as u32;
    for case_name in case_names {
        let entry = RecordWriter::extend_zero(blob, entry_layout);
        entry.write_slice(blob, INFO_TYPE_NAME, *type_name);
        entry.write_slice(blob, ENUM_INFO_CASE_NAME, *case_name);
    }
    Some(SymRef {
        target: segment_id,
        off: blob_off,
        len,
    })
}
