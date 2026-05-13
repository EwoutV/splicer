# otel-bare-logs

Tier-1 builtin that emits a structured `wasi:otel/logs` record on
every wrapped call. Each record carries the call's `(interface,
function)` as attributes, severity `INFO`, event-name `call.invoked`,
an `observed-timestamp`, and trace-correlation fields populated from
the host's `outer-span-context` when one is active. No
payload-derived content.

**Audience:** shops with a structured-logging backend (Loki, ELK,
Splunk) but no tracing pipeline — they want call-event records
flowing through the format their existing tooling consumes,
independent of whether they also collect spans.

## Config keys

None — this builtin reads no values from `splicer:builtin-config`.
Reference it in YAML as a short-form builtin:

```yaml
inject:
  - builtin: otel-bare-logs
```
