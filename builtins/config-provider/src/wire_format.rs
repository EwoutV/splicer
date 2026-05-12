// Shared wire-format constants and codec between the config-provider
// template (`builtins/config-provider/src/lib.rs`) and the splicer-
// side patcher (`src/config_provider.rs`). Both files `include!` this
// file; do not edit it in just one place.
//
// IMPORTANT: in the provider template, `MAGIC_BYTES` MUST only be
// referenced in const-eval contexts (inside `const` / `static`
// initializers). A runtime `&MAGIC_BYTES` forces rustc to emit the
// bytes as a separately-addressable static next to the byte-identical
// prefix of `SPLICER_CONFIG_BLOB`, which trips the patcher's
// "magic appears exactly once" check.

/// Sentinel the splicer-side patcher byte-scans for to locate the KV
/// buffer in the built component. Picked to be very unlikely to appear
/// in any other section (non-ASCII wrapper bytes around an ASCII tag).
pub(crate) const MAGIC_BYTES: [u8; 29] = *b"\x00\xefSPLICER_BUILTIN_CONFIG_V1\xef\x00";

/// Byte length of the magic sentinel.
pub(crate) const MAGIC_LEN: usize = MAGIC_BYTES.len();

/// Total bytes reserved for the magic sentinel + length header +
/// serialized table + padding. Patching fails if the serialized
/// table doesn't fit.
pub(crate) const CAPACITY: usize = 16 * 1024;

/// Width of every length-prefix field in the wire format
/// (`payload_len`, `count`, `key_len`, `val_len`).
pub(crate) const LEN_PREFIX_BYTES: usize = std::mem::size_of::<u32>();

/// Serialize a key-value table into the on-wire payload. Returns the
/// inner-payload bytes (the `MAGIC` and `payload_len` framing are
/// added by the caller). Sorted iteration over a `BTreeMap` gives
/// byte-deterministic output — two builds with the same `values`
/// produce identical patched-provider bytes.
///
/// Format: `[u32 LE count][u32 LE key_len][key bytes][u32 LE val_len][val bytes]...`
pub(crate) fn serialize_table(values: &std::collections::BTreeMap<String, String>) -> Vec<u8> {
    let count = values.len() as u32;
    let mut buf = Vec::new();
    buf.extend_from_slice(&count.to_le_bytes());
    for (k, v) in values {
        let kb = k.as_bytes();
        let vb = v.as_bytes();
        buf.extend_from_slice(&(kb.len() as u32).to_le_bytes());
        buf.extend_from_slice(kb);
        buf.extend_from_slice(&(vb.len() as u32).to_le_bytes());
        buf.extend_from_slice(vb);
    }
    buf
}

/// Deserialize an on-wire payload back to a `HashMap`. Returns an
/// empty map on any malformed framing (bad length, truncated entry,
/// non-UTF-8 string): the wire format is internal to splicer, so a
/// malformed table signals a build/patch bug rather than user input.
/// Callers fall back to per-builtin defaults regardless.
///
/// `#[allow(dead_code)]` since this is called by the patcher in `splicer/src` only.
#[allow(dead_code)]
pub(crate) fn deserialize_table(payload: &[u8]) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    let Some(count) = read_u32_le(payload, 0) else {
        return out;
    };
    let mut cursor = LEN_PREFIX_BYTES;
    for _ in 0..count {
        let Some(key_len) = read_u32_le(payload, cursor) else {
            return out;
        };
        cursor += LEN_PREFIX_BYTES;
        let key_end = cursor + key_len as usize;
        if key_end > payload.len() {
            return out;
        }
        let Ok(key) = std::str::from_utf8(&payload[cursor..key_end]) else {
            return out;
        };
        cursor = key_end;

        let Some(val_len) = read_u32_le(payload, cursor) else {
            return out;
        };
        cursor += LEN_PREFIX_BYTES;
        let val_end = cursor + val_len as usize;
        if val_end > payload.len() {
            return out;
        }
        let Ok(val) = std::str::from_utf8(&payload[cursor..val_end]) else {
            return out;
        };
        cursor = val_end;

        out.insert(key.to_string(), val.to_string());
    }
    out
}

/// Read a little-endian u32 at `off`. Returns `None` if the slice is
/// too short.
pub(crate) fn read_u32_le(buf: &[u8], off: usize) -> Option<u32> {
    let end = off.checked_add(LEN_PREFIX_BYTES)?;
    if end > buf.len() {
        return None;
    }
    Some(u32::from_le_bytes(buf[off..end].try_into().ok()?))
}
