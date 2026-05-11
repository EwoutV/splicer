# Tier 2: Observation

**Status:** shipped.

The middleware can see the function name, the types of the parameters
and return values, and the actual data being passed, but _cannot
modify_ any of it. The call flows through to the downstream unchanged;
_the middleware only observes_.

For the shared framework that applies to every tier (one-tier-
per-middleware rule, async convention, hook-trap propagation,
chain composition), see [`adapter-components.md`](../adapter-components.md).

## Value representation: flattened cells with side tables

The adapter lifts canonical-ABI values into a **flat array of cells**.
Compound cells reference children by `u32` index into the same array
rather than by direct self-reference. Nominal-typed metadata
(record-of, variant-case, etc.) lives in **per-kind side tables**
that cells reference by `u32` index — these are the `record-infos`
/ `flags-infos` / `enum-infos` / `variant-infos` / `handle-infos`
lists on `field-tree` in the WIT; the codegen docs call them "side
tables", the WIT names them `*-infos`, same thing either way. A
helper library (or hand-rolled walker) presents this as a tree;
the wire format itself is a single linear `list<cell>` plus a
small set of side lists plus a root index.

Two design constraints shape this layout:

1. **WIT does not yet support recursive types**
   ([component-model
   issue #56](https://github.com/WebAssembly/component-model/issues/56)),
   so the tree's recursion is "compiled out" into an index-keyed array.
2. **Canonical-ABI lays out a `list<variant>` as fixed-stride memory**,
   where the stride equals the variant's max payload size padded to
   alignment. Keeping nominal metadata inline in cell variant cases
   would force every cell — including a `cell::bool(true)` — to pay
   24+ bytes of padding. Pulling the metadata into side tables caps
   every cell payload at 8 bytes (`s64`), so the cell stride lands at
   16 bytes regardless of which case is present.

The `cell` variant is a tagged union over three families of cases:

- **Primitive** cases (`bool`, `integer`, `floating`, `text`, `bytes`)
  hold their payload inline. `integer` widens every signed/unsigned
  width up to s64; `floating` widens f32 to f64; `text` carries both
  `string` and `char`; `bytes` is the `list<u8>` fast-path.
- **Structural** cases (`list-of`, `tuple-of`, `option-some`,
  `option-none`, `result-ok`, `result-err`) carry **child cell-
  indices** into the same cells array — never inline payloads.
- **Nominal** cases (`record-of`, `flags-set`, `enum-case`,
  `variant-case`, and the four `*-handle`s — `resource-handle`,
  `stream-handle`, `future-handle`, `error-context-handle`) carry
  a `u32` index into the per-kind side table that holds the
  type-name + structural metadata (record field tuples, enum/
  variant case-name, flag-set bit names, handle correlation id).

A `field-tree` bundles the cells slab, one side-table slice per
nominal kind, and the `root` cell index. A `field` wraps a tree
with the param name. A function's args surface as `list<field>`;
the result as `option<field-tree>` (none for void; results are
unnamed in WIT, so no `field` wrapper).

The authoritative schema lives in
[`wit/common/world.wit`](../../wit/common/world.wit).

Every WIT type constructor maps to a distinct `cell` variant case, so
the lifted value is self-describing — middleware code can pattern-match
exhaustively without consulting the schema, and a generic trace
consumer can render a value correctly even without the WIT.

### Memory savings from the side-table split

Without the split (metadata inline in cell variant cases), the
cell stride is dominated by the largest case's payload size
padded to alignment — every cell pays for the worst case
regardless of which case is present. Pulling nominal metadata
into side tables caps every cell payload at 8 bytes (`s64`),
collapsing the stride to whatever the smallest viable shape
allows.

For primitive-dominated trees (the realistic shape — record
leaves are mostly primitives) this is roughly **50% savings**
vs. the inline alternative. Record-heavy trees roughly break
even, because each nominal cell trades the padding it would have
paid for a side-table entry of comparable size. Never
meaningfully worse, often dramatically better.

The cost is one extra `tree.<kind>_infos[idx]` lookup in middleware
code per nominal cell. Helper libraries hide this; without one, the
indirection is mechanical.

Type names inside cells use **simple** names (`"color"`, not
`"my:pkg/types@1.0.0.color"`). The fully-qualified interface identity
surfaces at the **call** level; tier-2's per-call hook receives the
fully-qualified interface plus the function name, so simple names
inside values are always unambiguous.

The adapter handles all canonical-ABI lifting; the middleware works
entirely with the cell representation. Tools that want a flat string
can format the tree themselves; tools that want structured access
(jsonpath-style metric extraction, schema-aware routing) can walk the
tree directly. Splicer emits one format and lets the tool decide what
to do with it.

## Resource, stream, future, and error-context handles

Resource, stream, future, and error-context handles all surface as
opaque `handle-info { type-name, id }` correlation records
(`resource-handle`, `stream-handle`, `future-handle`,
`error-context-handle`). The type-name identifies the kind
(`"request"`, `"u8"` for `stream<u8>`, `"response"` for
`future<response>`; **empty** for `error-context` — the cell-disc
already names the kind and there is no nested type to surface). The
`u64` is **not** a usable handle. The middleware cannot invoke methods
on it, read its contents, escape it past the call boundary, or drop
it. The adapter still owns canonical-ABI ownership semantics
(`own<R>`'s drop, `borrow<R>`'s lifetime, stream/future cleanup); the
ID is purely for reasoning about identity (e.g. "this `request` was
seen on `handle` and again as the parent of the `body` resource three
calls later").

### `error-context` is id-only — host limitation

The canonical ABI defines `error-context.debug-message`, which would
let the wrapper read the debug string in its own component (no
cross-component hop needed for the string itself). We deliberately
do **not** call it today: wasmtime currently ships an incomplete
`error-context` implementation ("very incomplete" per its own
`wasm_component_model_error_context` config docstring). The
`error_context_transfer` libcall fired by the FACT trampoline
crashes with `unknown handle index` when an `error-context`
crosses any component boundary — including a wrapper interposed
via splicer or even a wac-shim in a fan-in topology. The wrapper's
wasm code never runs, so option-2 (read the debug message) cannot
be validated end-to-end on current hosts.

When wasmtime's error-context support matures, this will be upgraded
to surface the debug string. The on-the-wire shape will change (likely
a sibling cell variant, e.g. `error-context-message(string)`, or
extending `handle-info.type-name` to carry the message); middleware
that pattern-matches on `error-context-handle` today should be ready
to switch.

### Oversized lists trap

The lift codegen reserves slabs sized `count * elem_bytes` (cells
slab) and `len * 4` (per-list child-index buffer). Both go through
`cabi_realloc`, whose size param is canonical-ABI i32 (signed). When a
dynamic `len` would make the multiplication overflow signed i32 — at
roughly 134M cells (16-byte cell stride) or 536M list-of indices — the
wrapper traps via `unreachable` rather than wrapping silently and
under-allocating.

In practice this fires only on pathological / adversarial inputs. The
trade-off is **trap (loud, clean abort) vs. clip (truncate the lifted
view, keep the call running)**: clipping would let observability
survive larger inputs, but creates a divergence between what the
handler sees (full list) and what the audit log records (truncated) —
exactly the property auditors don't want. The wire format is not yet
designed for a "this list was truncated" marker, so today's behavior
is trap.

If your deployment surfaces traps from large lists, file a bug with
the call shape; the policy is revisitable but warrants real call-size
data before changing.

### What this means for resource-bearing target interfaces

Tier-2 lifting is bounded by what the canonical ABI exposes. For
target interfaces that pass resources by handle (e.g.
`wasi:http/handler@0.3.0`'s `handle: async func(request: request) -> ...`),
the middleware sees only the handle — not the request's headers,
method, body, or any other contents. The contents live behind methods
on the resource that the wasi:http host implements; from the
middleware's vantage point at the `handler` boundary, those are
unreachable.

To observe what's *inside* a resource, you have three paths:

1. **Multi-WIT instrumentation (recommended).** Apply tier-2 to **both**
   `wasi:http/handler` (sees the top-level call) **and**
   `wasi:http/types` (sees every method invocation on the request /
   response / headers / body resources). Correlate by handle id —
   `("request", 42)` at the handler boundary is the same logical
   request as `("request", 42)` flowing as `self` into
   `[method]request.headers`. Reconstruct the picture from the call
   stream. This is the canonical recorder pattern.
2. **Specialized middleware** (loses target-agnosticism). The
   middleware imports `wasi:http/types` directly and calls methods on
   the handles it receives. Now the middleware is HTTP-specific, not
   reusable across interfaces.
3. **Don't observe the contents.** A throttler / tracer / circuit
   breaker that only cares about call shape and handle correlation
   doesn't need to peer inside.

A future UX improvement (tracked in
[`docs/TODO/adapter-comp-planning.md`](../TODO/adapter-comp-planning.md))
is an `instrument-resources: true` rule modifier that auto-attaches
the same middleware to the resource-defining interface alongside the
target. For now, multi-WIT setup is explicit.

### Stream / future content observation

For streaming protocols where the middleware actually wants to observe
**content** (e.g. logging an HTTP body element-by-element), tier-2 v1
deliberately does **not** support that. It's planned as a separate
opt-in interface (`splicer:tier2/stream-observer`) once a concrete use
case justifies the implementation cost.

## Tier-2 hook interfaces

The tier-2 WIT package mirrors tier-1's split-by-hook structure:
two interfaces, `before` and `after`. `before::on-call(call,
args)` carries the function's params as a `list<field>`;
`after::on-return(call, result)` carries the return as
`option<field-tree>` (`none` for void). Both hooks are `async`.

**Receiver convention.** For resource methods (`request.body()`, etc.),
the receiver `borrow<request>` / `own<request>` surfaces as the first
entry in `args` with `name: "self"`. The remaining declared parameters
follow in WIT-declaration order.

**Function naming.** `call-id.function-name` uses the **canonical-ABI**
function name verbatim — `"[constructor]request"`,
`"[method]request.body"`, `"[static]request.from-uri"`, `"handle"` for
plain functions. No special-casing or pretty-printing; the middleware
sees what the canonical ABI sees.

A middleware can export any non-empty subset:

- `before` only — pre-call observation (e.g. throttler that counts inbound shapes)
- `after` only — post-call observation (e.g. response logger)
- `before` + `after` — full lifecycle (e.g. tracer, recorder, metrics)

The adapter only fires hooks the middleware actually exports, so a
`before`-only middleware never pays the lift cost on the result.

**Result representation.** WIT functions have at most one result and
results are unnamed, so `on-return` carries `option<field-tree>`
directly (`none` for void functions, `some(tree)` otherwise) rather
than wrapping in a `field` with a synthetic name.

**WIT definition:** [`wit/tier2/world.wit`](../../wit/tier2/world.wit)

### Future hook: `on-trap`

A trap-observability hook (`on-trap(call, reason)`) was scoped but
intentionally not shipped. The motivating use case is real:
instrumenting a target interface and seeing when a downstream call
fails. The blocker is at the runtime layer rather than our codegen:
canon-async on current wasmtime releases propagates child-task
traps as wasm traps that unwind the parent's stack — the parent
guest never gets a chance to observe the trap before unwinding
alongside it. There's no `Status::Failed` or `Event::TaskFailed`
for the parent's wait-loop to dispatch on, neither for guest-
implemented nor host-implemented targets.

Wiring `on-trap` would require either (1) canon-async growing a
guest-visible terminal-error event the parent can poll for, or (2)
the adapter wrapping every async call in an exception-catching
shell — neither is in scope today. We'll add the WIT + dispatch
when upstream lands the event semantics.

**Good for:** request/response logging with payload inspection, metrics
extraction from request fields, content-based routing decisions,
throttling by request shape, authentication/authorization, security
policy enforcement, parameter validation. When applied at multiple WIT
boundaries simultaneously (e.g. `wasi:http/handler` plus
`wasi:http/types`), tier-2 also enables **span-based recording**: the
middleware can correlate the resource handles that surface across
nested calls within a single top-level invocation, then log the entire
causal trace as one record.

## Planned: resource-shape adapter-adapter

The cell-array wire format is chosen for **performance and polyglot
neutrality**: a single canonical-ABI lower per call, no per-language
helper library required *to be correct*. But the index-walking pattern
is awkward to write directly; languages without a splicer-provided
walker library will find the cells gnarly.

The plan is to ship a **second WIT package**, `splicer:tier2-resources`,
that exposes the same observation hooks but with the lifted value
wrapped as a `resource lifted-value` with lazy accessor methods
(`kind()`, `as-integer()`, `as-list() -> list<lifted-value>`, etc.).
Resource bindings are first-class in every wit-bindgen target, so
middleware authors writing in TS, Python, Go, or any other language
get an idiomatic API without splicer needing to ship per-language
helpers.

The bridge will be an **adapter-adapter component** that splicer ships
and auto-wires when it detects a middleware exporting
`splicer:tier2-resources/*`:

```
caller
  → splicer's tier-2 adapter  (lifts to cells, the canonical wire format)
      → adapter-adapter        (cells → resource methods, opt-in)
          → user middleware
              → handler
```

This pattern gives both worlds:

- **Default (cells, fast)**: middleware exports `splicer:tier2/*`,
  consumes the cell array directly, walks with the splicer-supplied
  Rust helper crate or its own walker. Single canonical-ABI lower per
  call, in-process traversal — sub-microsecond walk for typical
  HTTP-scale payloads.
- **Ergonomic (resources, polyglot)**: middleware exports
  `splicer:tier2-resources/*`, never touches indices. Works
  idiomatically in every language without a splicer-provided helper.

  **Runtime cost** (the price of opting in): every accessor on
  `lifted-value` is a component-boundary call. Walking a 50-field
  record is ~150 boundary crossings (~5–10 μs at wasmtime's current
  overhead) vs. ~250 ns for the direct-cells path — roughly **30×
  slower per walk**. For light-touch middleware (auth, throttling,
  tracer reading a few fields) this is irrelevant. For
  traversal-heavy middleware (logger, recorder dumping the entire
  tree) it's meaningful — at HTTP scale (~10 ms request budget),
  ~0.1% added latency per traversal, still acceptable for most use
  cases. If perf matters, drop the adapter-adapter and walk cells
  directly.

Not in scope for tier-2 v1; the cell wire format is forward-compatible
with this shim landing later.
