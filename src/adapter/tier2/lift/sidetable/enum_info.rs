//! Enum-info side-table builder. One entry per case in the
//! `enum-info` segment; the cell points at the contiguous range.

use super::super::super::super::abi::emit::RecordLayout;
use super::super::super::blob::{RecordWriter, Segment, SymRef, SymbolId};
use super::super::super::FuncClassified;
use super::super::classify::ResultSource;
use super::super::plan::{Cell, LiftPlan};
use super::{SideTableBlob, INFO_TYPE_NAME};

const ENUM_INFO_CASE_NAME: &str = "case-name";

/// Build the enum-info segment. Cell at runtime points at the
/// per-(param|result) range via `(blob_off, len)`.
///
/// **≤1 `EnumCase` per param/result** — cell payload is the case disc
/// (not an offset), so two enums per range would collide. The
/// debug_assert below catches violations.
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
        // Compound: walk plan (catches enums in list element plans).
        // Direct: the cell IS the result.
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

/// The plan's sole `Cell::EnumCase` (walks into list element plans
/// since they're the only sub-`LiftPlan` carrier). debug_asserts ≤1.
fn sole_enum_case(plan: &LiftPlan) -> Option<&Cell> {
    let mut found: Option<&Cell> = None;
    visit_enum_cases(plan, &mut |cell| {
        debug_assert!(
            found.is_none(),
            "≤1 EnumCase per range; see build_enum_info_blob doc"
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
