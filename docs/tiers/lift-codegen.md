# Adapter lift codegen

Design and rationale behind tier-2's lift codegen. The wrapper
sits between importer and exporter and emits an observation the
middleware can read; this doc is for contributors editing
`src/adapter/tier2/`.

The wire format (cell shape, field-tree fields, side-table records)
is specified in [`tier-2.md`](./tier-2.md). Implementation details
(struct shapes, match arms, helper names) live in the code; they
rot fastest and are easiest to read at the source.

> **Scope.** Tier-2 today. Tier-1 hooks pass raw canonical-ABI
> bytes through with no lift. Tier-3+ is not yet implemented; when
> it lands, extend this doc with its model.

---

## What this codegen does

The component model's **canonical ABI flattens** every typed WIT
value into a sequence of primitive wasm flat slots (i32 / i64 /
f32 / f64) when crossing a function boundary. Compound values
either fit in flat slots (small records, tuples) or get spilled
to a caller-provided "retptr" memory region. By the time control
reaches our wrapper, the typed value has already been reduced to
that flat shape — there is no typed Rust struct to inspect, just
wasm locals and pointers.

**Lift codegen produces wasm that re-typifies the flat input into
a structured observation** the middleware reads. For each value
position in the WIT signature the wrapper writes one **cell** — a
tagged-union record naming the kind (`bool`, `text`, `record-of`,
`variant-case`, `*-handle`, …) and a payload (a primitive value,
a `(ptr, len)` slice, an index into a side table). Cells for one
(function, param | result) sit contiguously in a **cells slab**;
a **field-tree** points at the slab plus per-kind **side tables**
carrying build-time-const metadata like type-names and case-name
lists. The side tables are the `record-infos` / `flags-infos` /
`enum-infos` / `variant-infos` / `handle-infos` lists on
`field-tree` in `wit/common/world.wit` — this doc calls them
"side tables", the WIT names them `*-infos`, same thing either way.

Compound cells reference their children as **siblings in the same
slab** via `cell-idx` rather than embedding payloads inline — so
records, tuples, options, results, variants, and list elements
are all addressed uniformly. Lists are dynamically sized: their cell
payload points at a `cabi_realloc`'d sub-slab the wrapper grew at
call time. The field-tree is **observation, not value
transformation** — the underlying canonical-ABI flat values pass
through to the handler unchanged.

---

## Pipeline

For each target function the adapter walks three phases:

```
WIT param/result types
       ↓ classify
  LiftPlan (per param, per result)
       ↓ layout
  static memory (cells slabs, side tables, fields blob)
       ↓ emit
  per-fn wrapper that lifts → $on_call → forwards → $on_return
```

Schema layouts (`field`, `field-tree`, `cell`, side-table records)
are derived once via `wit_parser::SizeAlign` in `compute_schema`
and threaded as `SchemaLayouts`. Field offsets are looked up by
name at use sites — no hardcoded offsets in adapter codegen.

---

## Library leverage

We consume `wit-parser` / `wit-bindgen-core` as **leaf operations**:
`SizeAlign` for sizes / aligns / field offsets, `Resolve::push_flat`
for canonical-ABI flat-slot counts, `wit_parser::abi::join` for
variant payload joining, and `wit_bindgen_core::abi::lift_from_memory`
to walk a WIT type from a memory address and push flat values onto
the stack (used for retptr-loaded compound results and async
`task.return` flat loads). `lower_to_memory` is the inverse and is
anticipated for tier-3's modify-and-write-back path.

We do **not** use `wit_bindgen_core::abi::call(...)` as a wrapper
driver. It's built around a typed recursive `Value` representation
that WIT doesn't support and that the cell format was specifically
designed to avoid. The hand-rolled `LiftPlan` is the right fit.

---

## The LiftPlan: a flat IR

Classification produces a `LiftPlan` per (function, param) and per
(function, result): a flat `Vec<Cell>` describing every cell the
lift writes, in allocation order. The cell variants live in the
code — read them there. This section is about why the IR is shaped
the way it is.

### Three invariants

1. **Single source of truth for cell count.** `plan.cells.len()`
   is what we allocate slab space for. A field-tree with
   `cells.len = 5` requires 5 cells, full stop.
2. **Cell indices are vector positions.** When a parent cell
   references a child by index, that index is literally the
   position the child got pushed to during plan-building. The
   same vector that codegen iterates over at emit time is the
   one side-table entries reference — they can't disagree
   because they read the same vector.
3. **Flat-slot consumption is `plan.flat_slot_count`.** No
   per-kind `slot_count()` table to keep in sync with the
   plan-builder; the builder's cursor at `into_plan` time *is*
   the count.

### Children-first allocation

`LiftPlanBuilder::push` allocates children first: each sub-call
appends its own cells and returns the index where its root landed;
the caller pushes the parent referencing those already-known
indices. Parents are appended fully constructed — no mutation
after push, no back-fill.

A consequence: **the root cell lands at the last index in `cells`,
not the first** (leaves go in first; the root is constructed and
pushed last). `LiftPlan::root` records where the root actually
landed, and the wire format's `field-tree.root` field carries the
same number — so consumers follow `tree.root` and land on the
root wherever it sits, rather than assuming it's at `cells[0]`.

### Why cells store offsets, not absolute locals

Each WIT param gets its own `LiftPlan`, and the result has its own
plan too — different types produce different plans. But every plan
is **built at classify time**, before the layout phase has decided
where in the wasm function's locals each param's flat slice will
sit. And once built, each plan is **read by multiple consumers**:
the emitter (which writes the lift bytecode) and every side-table
builder (which collects per-kind metadata like type-names and
child-cell indices).

The locals layout matters here because one WIT param can consume
many wasm flat slots: `u32` is 1; `string` is 2 — `(ptr, len)`;
`record { a: u32, b: string }` is 3; and so on. A function's wasm
locals end up as a sequence of variable-width per-param slices.
The result, when compound, lives in its own slice of synth locals.

Since plans are built before any slice positions are known, cells
store flat-slot positions as **offsets** in `0..flat_slot_count`,
not absolute wasm-local indices. At emit time the caller computes
a **`local_base`** — the absolute wasm-local index where this
plan's flat-slot slice starts — and passes it in. The per-cell
formula is then just `local_base + offset(N)`; the cumulative-sum
work happens once, when `local_base` is computed for this emit
call, not per cell.

Where `local_base` comes from per destination:

- **Param emit**: a cumulative cursor over preceding params'
  flat-slot counts. Param 0's `local_base` is 0; param 1's is
  `params[0].flat_slot_count`; param 2's is
  `params[0].flat_slot_count + params[1].flat_slot_count`; etc.
- **Compound-result emit**: the first of `flat_slot_count` synth
  locals the wrapper allocates to receive `lift_from_memory`'s
  output. The "synth locals are contiguous" invariant lives at the
  alloc site, so cell N's absolute local is `synth_locals[0] + N`.
- **Side-table builders**: no `local_base` — they only read
  structural fields like case names and child indices.

---

## Side-table storage: static vs per-call

Nominal cells (`enum-case`, `record-of`, `flags-set`, `variant-case`,
`*-handle`, …) carry a `u32` *side-table index*. The metadata
(type-names, case-names, child cell indices, etc.) lives in per-kind
side tables on the field-tree.

Two timelines matter for storage decisions:

- **Build time** — when splicer generates the adapter wasm. A Rust
  program walks the WIT and decides what bytes go into the binary.
  Anything known here is **static** and can be baked directly into
  the wasm (as data-segment bytes or `i32.const`s).
- **Execution time** — when the generated adapter wasm actually
  executes a call. Anything only knowable here is **dynamic** and
  has to be computed by, or read from, the running wasm.

Each side-table entry mixes both kinds of fields:

- **Static fields** come straight from the WIT and are known when
  splicer generates the adapter: a `record-info`'s `type-name`
  (`"point"`), the list of case-names on a variant (`"circle"`,
  `"square"`, …), child-cell indices for a record's fields.
- **Dynamic fields** are only known when the wrapper executes: a
  `handle-info`'s `id: u64` (handed in by the host), a
  `variant-info`'s actual `case-name` (one of the static
  case-names, but *which* one depends on the dynamic disc value),
  a `flags-info`'s `set-flags` list (which bits are set depends on
  the actual argument). These have to be written per call by the
  running wasm.

Two storage policies, picked per kind:

- **Static segment, dynamic fields patched per call.** Side-table
  entries live in a wasm data segment baked at build time. Static
  fields are written into the segment bytes directly; dynamic
  fields are patched per call by the running wrapper into
  pre-reserved slots in the same entry. The field-tree's
  `<kind>-infos` slice is baked with both `(ptr, len)` since the
  count is known statically.
- **Per-call buffer, every field written per call.** No pre-baked
  bytes — the wrapper calls `cabi_realloc` per call to allocate a
  buffer, then writes *every* field of every entry. Static fields
  come out as `i32.const`s emitted into the wrapper body; dynamic
  fields come from wasm locals. Two sub-regimes per (fn, param |
  result):
  - *Static count*: no list-of-this-kind in the plan. Buffer size
    is `static_count * sizeof(entry)`, fixed at build time;
    `<kind>-infos.len` is baked, only `.ptr` is patched per call.
  - *Dynamic count*: at least one list element is of this kind.
    The pre-pass accumulates `static_count + Σ_lists len *
    entries_per_elem` into the `cabi_realloc` size; both `.ptr`
    and `.len` are patched per call. Per-list base is captured
    before bumping; per-iteration `list_elem_<kind>_base = base +
    j * entries_per_elem` resolves the absolute slot for each
    element-plan cell.

**Rule.** A side-table kind moves to the per-call policy as soon
as any element-plan can introduce a list-of-that-kind: the entry
count then depends on the dynamic list length, and a static data
segment can't be sized for that.

Per-call storage costs ~7 wasm instructions + one extra
`cabi_realloc` per (fn, param | result) with that kind, plus
per-cell write-overhead from fields that used to be statically
baked. Paid uniformly even when the list-of-form isn't in play.
The tradeoff favors uniform shape over the few instructions saved
by a hybrid path.

### Static-vs-dynamic discriminant indices

For some kinds the side-table index is dynamic (e.g., `enum-case`:
the disc selects an entry); for others it's static (e.g.,
`record-of`: one entry per plan cell, known at build time). For
list-element kinds the index is dynamically staged off a per-
iteration base. `PayloadSource::{Local, ConstI32}` in `cells.rs`
discriminates at byte emission; the codegen layer picks the source
based on the cell's semantics.

---

## Wrapper-body shape

For each target function the wrapper has four sub-phases:

```text
on-call:    lift every cell in plan order → call $on_call hook
forward:    bridge callee-returns ↔ caller-allocates retptr;
            call handler
on-return:  if result lift exists, lift the result cell(s) →
            call $on_return hook
tail:       async task.return (with retptr or flat) or sync return
```

The cell-write loop is the heart of lift codegen: for each
`(cell_idx, cell)` in `plan.cells`, compute
`cell_addr = cells_offset + cell_idx * cell_size`, then dispatch
through `emit_cell_op` to `CellLayout::emit_<kind>` in `cells.rs`.
The emit helpers are ABI-correct by construction — they read field
offsets from the schema-derived `CellLayout` and use the
`PayloadPart` abstraction to enforce store-width and alignment.

Per-fn locals (`addr` scratch, canon-async status / waitable-set
handles, retptr-loaded `(ptr, len)` scratch, widening locals for
sign/zero-extension and f32→f64 promotion, list-iteration bases,
per-call info-buffer pointers, …) are pre-allocated in
`alloc_wrapper_locals` and referenced through `WrapperLocals` —
not raw `u32`s — at every emit site, so the deterministic
allocation order isn't load-bearing for correctness.

---

## Adding a new kind

The per-type workflow lives in
[`../TODO/tier2-list-compound-elements.md`](../TODO/tier2-list-compound-elements.md).
Short version of the touched surfaces: plan-builder arm
(`LiftPlanBuilder::push`) → cell-emit helper in `cells.rs` →
emit-phase dispatch in `lift/emit.rs::emit_cell_op` → side-table
builder under `lift/sidetable/` if the kind carries per-cell info
(new per-cell maps go through `PerCellIndices<T>`) → tests at
cell-emit / plan-shape / integration / canned-shape levels.

If the kind can appear as a top-level result, also extend
`is_compound_result` in `lift/classify.rs` (multi-cell kinds take
Compound; single-cell kinds take Direct via `single_cell_for_result`
+ `is_supported_direct_result`).

---

## Out of scope

- **Tier-1 codegen** (`src/adapter/tier1/`) — different model
  entirely, no lift; passes raw canonical-ABI bytes through.
- **The `field-tree` wire format** for middleware authors — see
  [`tier-2.md`](./tier-2.md).
- **Tier-3+ models** — not yet implemented.
