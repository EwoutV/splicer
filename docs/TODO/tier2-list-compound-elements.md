# Tier-2 lift: outstanding payload-type extensions

Every other type lifts today (primitives, `string`, `list<u8>`,
`enum`, `record`, `tuple`, `option`, `result`, `flags`, `variant`,
`char`, `own<R>` / `borrow<R>`, `stream<T>` / `future<T>`,
`error-context`, and `list<T>` over every kind except nested
lists).

## What's left

### `list<list<T>>` (tier-3, list-element gate)

The only gated list-element kind. `Cell::allowed_as_list_element`
(`lift/plan.rs`) is the gate; the bail fires at `push_list_of` and
is pinned by `nested_list_bails_at_plan_build` in `lift/tests.rs`.

Child indices live in static side-table segments that assume
build-time-known absolute indices. Lifting needs either per-call
dynamic side-table growth (the route in use for `list<own<R>>` /
`list<record>` / `list<variant>` etc. via per-call info buffers)
or a schema-level "template + per-instance base." Multi-day
recursive-design pass — settle the design before promoting.

### `FixedLengthList` / `Map` typedefs

Guarded by `todo!()` in `lift/plan.rs`'s payload-type match
(below the `Resource` / `Unknown` `unreachable!()`s).

- `FixedLengthList(elem, N)` — list with a build-time-known
  length. Smallest remaining gap: no `len` operand, no per-call
  buffer dance. Likely reusable through `push_list_of` with a
  synthetic constant `len`, or a new `Cell::FixedLengthListOf`
  iterating over a static stride.
- `Map(K, V)` — defer until something in the wild needs it; the
  lower side has stubs but the lift design isn't pinned.

## Per-type workflow (for future kinds)

Five phases, each its own commit so review is small and bisect
stays narrow.

1. **Params** — plan-builder arm (`LiftPlanBuilder::push`, drop
   the matching `todo!()`), cell emit helper in `cells.rs`,
   emit-phase dispatch in `lift/emit.rs::emit_cell_op`,
   side-table builder under `lift/sidetable/` if needed (new
   per-cell maps go through [`PerCellIndices<T>`]). Tests:
   cell-emit unit test, plan-shape tests covering leaf /
   nested-in-record / nested-in-self, integration roundtrip.
2. **Self-review params** — walk every touched file: stale
   comments, edge cases (empty / single-element / zero-flat-slot),
   nested `Vec<Vec<…>>` without typed accessors, `expect()`s on
   new-path invariants.
3. **Results** — extend `is_compound_result` in `lift/classify.rs`.
   Multi-cell kinds take Compound; single-cell take Direct via
   `single_cell_for_result` + `is_supported_direct_result`. Pin
   the single-flat-slot fall-through (`tuple<u32>`,
   `record { a: u32 }`) at `result_at_retptr`.
4. **Self-review results** — same as phase 2 with attention to
   retptr + `lift_from_memory` + synth-locals. Runtime side-table
   growth (flags / variant / handle) needs extra care; grep for
   adapter-build-time-known cell-count assumptions.
5. **Canned shape** — new `Shape::<Kind>(…)` in `tier2_shapes()`
   (`tests/fuzz_and_run.rs`), `Cell::<Kind>(…)` arm in
   `MIDDLEWARE_TIER2_LIB_RS::fmt_cell`, `Shape::<Kind>` arm in
   `predict_tier2_arg_inner` matching the middleware's rendering
   character-for-character. Run via `cargo test --test
   fuzz_and_run test_tier2_canned -- --ignored` (filter with
   `SPLICER_RUNTIME_SHAPES=<name>`).

## Out of scope

- `Resource` / `Unknown` typedefs at payload position — both
  `unreachable!()` in `lift/plan.rs` (canon-ABI forbids bare
  resources; `Unknown` is wit-parser's unresolved sentinel).
- Direct-compound emit (lifting single-flat-slot compounds rather
  than dropping them to no-lift). Would let `tuple<u32>` and
  `record { a: u32 }` results show up in the after-hook; no use
  case has surfaced.

[`PerCellIndices<T>`]: ../../src/adapter/tier2/lift/sidetable/mod.rs
