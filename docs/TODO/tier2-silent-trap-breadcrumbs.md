# Tier-2 silent trap breadcrumbs

`emit_cabi_realloc_call_runtime` traps via `unreachable` when
`count_local * elem_bytes` would overflow signed i32 (~134M cells at
16-byte stride, ~536M list-of indices). The trap surfaces to the host
opaque — no signal of which list / which call shape blew the bound.
`emit_trap_if_list_overflows_cell_slab` has the same problem at its
per-list pre-check.

Today's post-mortem path is: read the dropped before-hook args dump,
eyeball the list lengths, guess. At production scale-out, that's not
enough — operators need a deterministic breadcrumb identifying the
offending site. Doesn't block β; worth landing before tier-2 sees
production traffic.

## Approach: name-section breadcrumbs

Factor each `unreachable` site into a tiny `() -> ()` wasm function
whose body is `unreachable; end`, register it in the wasm name section
under a descriptive identifier (e.g. `splicer_trap__<iface>.<fn>`), and
emit `call <stub_idx>` from the trap site instead of an inline
`unreachable`. When wasmtime catches the trap, its `Trap` already
surfaces a wasm backtrace including function names — operators get the
breadcrumb through the trap reporting they already have, with zero new
contract:

- No reserved-memory layout shared with embedders.
- No new host import (`splicer:debug/trap-reason` etc.) for embedders
  to wire.
- Works through wasmtime's existing trap-reporting infrastructure
  unchanged.

Two earlier sketches were considered and rejected:

- **Reserved sentinel address.** Trap site stores a code/string into a
  fixed memory slot before `unreachable`; host inspects on trap.
  Recoverable but invisible without embedder-side code that knows the
  reserved address. Splicer doesn't ship a host runner, so the
  breadcrumb is opaque to anyone who doesn't already have private
  contract knowledge.
- **`splicer:debug/trap-reason` host import.** Wrapper calls the
  import with a build-time-known string, then `unreachable`.
  User-visible only if the embedder wires the import; pushes work onto
  consumer infrastructure splicer doesn't ship.

Name-section breadcrumbs deliver the same debuggability through the
trap reporting wasmtime *already* gives the embedder.

## Cost

- One `() -> ()` function per stub (≈2 bytes of body + a small name
  entry). Per-wrapper at v1; per-trap-site at v2 if granularity needs
  it.
- A name section, which the dispatch core module doesn't emit today
  (one-time `wasm_encoder::NameSection` wiring; section gets appended
  after the code section per wasm spec).

## v1 implementation plan (one trap stub per wrapper)

All trap sites in a wrapper call into a single shared stub for that
wrapper. Backtrace shape: outer wrapper frame → stub frame → host
trap. Today's `UnreachableCodeReached at <opaque>` becomes "trap
inside `splicer_trap__user.lookup`" — enough to identify the call
shape without a pre-walk to count per-site stubs.

**Step 1 — extend section emission** (`src/adapter/tier2/section_emit.rs`):

- `emit_imports_and_funcs`: allocate one extra `() -> ()` slot per
  wrapper in `FunctionSection`. Reuse `canon_async::AsyncTypes::void_i32_ty`?
  No — that takes one i32 param. Need a fresh `() -> ()` type slot;
  add `trap_stub_ty: u32` to `TypeIndices`. Return
  `trap_stub_idx: Vec<u32>` (parallel to `per_func`) on `FuncIndices`.
- `emit_code_section`: after the wrapper bodies but before
  `cabi_realloc`, emit one stub body per wrapper:
  `f.instructions().unreachable(); f.instructions().end();`. Order
  must match `trap_stub_idx` exactly.
- New `emit_name_section(module, per_func, func_idx, ...)` that
  appends a `NameSection` AFTER `emit_code_section`. Names registered:
  - Each wrapper: `<iface>.<fn>` (already known from
    `FuncDispatch::export_name`).
  - Each trap stub: `splicer_trap__<iface>.<fn>`.
  - `cabi_realloc`, `_initialize`, each `cabi_post_*` shim — free
    debuggability while we're here.
  - Imports too, if cheap (`NameMap` indexing covers all funcs
    including imports). Skip if that bloats the section.

**Step 2 — wire stub index into trap helpers** (`src/adapter/abi/emit.rs`):

- `emit_cabi_realloc_call_runtime` and
  `emit_trap_if_list_overflows_cell_slab` each gain a
  `trap_stub_idx: u32` parameter. Inside their `if (overflow) {
  ... }` block, replace `f.instructions().unreachable();` with
  `f.instructions().call(trap_stub_idx);`. The stub itself traps; wasm
  validation accepts a `() -> ()` call as a block terminator (block
  type `Empty` matches stack post-call).
- Existing tests `cabi_realloc_runtime_emits_overflow_trap` and
  `list_overflow_trap_emits_unreachable_for_every_shape` count
  `unreachable` opcodes in the emitted body. Update them to count
  `call` to a known stub index instead, OR add a stub-fn at index 0 in
  the test module so `unreachable` lands inside the stub body (count
  stays 1 in stub body, 0 in caller body).

**Step 3 — thread stub index through wrapper-body emit**
(`src/adapter/tier2/wrapper_body.rs`):

- `WrapperCtx` gains `trap_stub_idx: u32` (the per-wrapper index;
  caller picks `trap_stub_idx[i]` from `FuncIndices`).
- `emit_alloc_cells_for_plan` plumbs it through to its trap-helper
  calls.
- `lift::emit::emit_list_of_arm` does the same.

**Step 4 — tests** (`src/adapter/tier2/`):

- Build a tier-2 module with a list-bearing fn, finish to bytes, parse
  with `wasmparser`, walk the name section, assert the expected
  `splicer_trap__<iface>.<fn>` name appears for the wrapper. Mirrors
  existing emit-helper unit-test style.
- Smoke test: assert `wasmparser::Validator::new().validate_all(&bytes)`
  still passes (the existing tier-2 integration tests cover this — no
  new wiring needed if they keep passing).

## v2 follow-up (separate landing)

Per-trap-site stubs distinguishing cells-slab realloc vs. per-list
cell-slab overflow vs. per-list indices realloc, plus the param/result
+ list index in the name. Needs a pre-walk in section_emit to count +
name stubs before `FunctionSection` emit; deferred until v1 ships and
we see whether per-wrapper granularity is enough in practice.

## Pointers

- Trap sites: `src/adapter/abi/emit.rs`
  (`emit_cabi_realloc_call_runtime`,
  `emit_trap_if_list_overflows_cell_slab`).
- Existing trap unit tests: same file
  (`cabi_realloc_runtime_emits_overflow_trap`,
  `list_overflow_trap_emits_unreachable_for_every_shape`).
- Module assembly: `src/adapter/tier2/section_emit.rs` (type +
  function + code sections).
- Wrapper bodies: `src/adapter/tier2/wrapper_body.rs`
  (`emit_alloc_cells_for_plan`'s per-plan cells-slab realloc) and
  `src/adapter/tier2/lift/emit.rs::emit_list_of_arm` (per-list indices
  realloc).
- wasm-encoder name section API: `wasm_encoder::NameSection` +
  `NameMap` (`functions`, `module`, `append`). Already pulled in by
  `wasm-encoder = "0.247"` in `Cargo.toml`.
