# hello-tier1

Tier-1 builtin that `println!`s a line per wrapped call. Lightweight
smoke for splice rules and the `splicer:builtin-config` substrate.

Output format:

```
[<greeting>] before <iface>#<fn>
[<greeting>] after  <iface>#<fn>
```

## Config keys

Read at first call via `splicer:builtin-config/get` and cached for
the rest of the wasm-instance lifetime.

| Key        | Type   | Default       | Description                                  |
|------------|--------|---------------|----------------------------------------------|
| `greeting` | string | `hello-tier1` | Replaces the bracketed prefix in each line.  |

Example splice config:

```yaml
inject:
  - builtin:
      name: hello-tier1
      config:
        greeting: "wired-up-greeting"
```
