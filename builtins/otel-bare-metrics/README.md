# otel-bare-metrics

Tier-1 builtin that emits `wasi:otel` count + duration metrics per
wrapped call, with call-id attributes (interface name, function name)
only. No payload-derived data.

`on-call` records the start time; `on-return` ships a fresh
delta-temporality `resource-metrics` payload with one data point per
metric (count + duration) via `wasi:otel/metrics.export`. The
downstream collector re-aggregates the deltas. Cheap to implement;
trades host-call frequency for simplicity.

## Config keys

None today — every call exports immediately
(effectively `buffer = 1`).

The planned aggregation rework, gated on the
`splicer:builtin-config` substrate, will add:

| Key                   | Type | Default | Description                                                                                 |
|-----------------------|------|---------|---------------------------------------------------------------------------------------------|
| `buffer`              | u32  | 1       | Accumulate N measurements per `(iface, fn)` before flushing. `1` is the current per-call behavior. |
| `flush_after_seconds` | f64  | 10.0    | Staleness flush trigger; ignored when `buffer == 1`.                                       |

See the `TODO(aggregation)` block at the top of `src/lib.rs` for the
full plan; until that lands, this builtin runs in always-flush mode.
