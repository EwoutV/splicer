//! Provider template for the `splicer:builtin-config` substrate.
//!
//! Builds into a generic wasm component exporting
//! `splicer:builtin-config/get`. The actual key/value table lives in
//! a fixed-capacity `static` buffer (`SPLICER_CONFIG_BLOB`) marked with
//! a known magic prefix; splicer's `config_provider::build_provider`
//! finds the magic by byte-scan in the built wasm and overwrites the
//! payload at splice time. Every inject site that wants config gets
//! its own patched copy of this component.
//!
//! Wire format inside the blob (after `MAGIC`):
//!   [u32 LE payload_len]
//!   [payload_len bytes:
//!     [u32 LE count]
//!     repeat count times:
//!       [u32 LE key_len][key bytes]
//!       [u32 LE val_len][val bytes]
//!   ]
//!   [padding to CAPACITY]
//!
//! Initial state (unpatched): `payload_len == 0`, so every `get`
//! returns `none` and consumers fall back to their hardcoded defaults.

mod bindings {
    wit_bindgen::generate!({
        world: "config-provider-mdl",
        generate_all,
    });
}

use std::collections::HashMap;
use std::sync::OnceLock;

use bindings::exports::splicer::builtin_config::get::Guest;

// Wire-format constants AND codec (serialize_table / deserialize_table
// / read_u32_le) shared with splicer's `src/config_provider.rs`, which
// loads the same file via `#[path = "..."] mod wire_format;`. Do not
// duplicate the format here.
//
// IMPORTANT: only reference `wire_format::MAGIC_BYTES` in const-eval
// contexts (inside `const` / `static` initializers). A runtime
// `&MAGIC_BYTES` forces rustc to emit the bytes as a separately-
// addressable static next to the byte-identical prefix of
// `SPLICER_CONFIG_BLOB`, which the splicer-side byte-scan then sees
// as a duplicate match.
mod wire_format;

use wire_format::{
    deserialize_table, read_u32_le, CAPACITY, LEN_PREFIX_BYTES, MAGIC_BYTES, MAGIC_LEN,
};

/// Storage for the splice-time KV table. The first `MAGIC_LEN` bytes
/// are the sentinel the splicer-side patcher byte-scans for; the
/// next `LEN_PREFIX_BYTES` are the payload length (little-endian);
/// the rest holds the serialized table followed by 0xAA padding.
#[no_mangle]
pub static SPLICER_CONFIG_BLOB: [u8; CAPACITY] = {
    let mut b = [0xAA_u8; CAPACITY];
    let mut i = 0;
    while i < MAGIC_BYTES.len() {
        b[i] = MAGIC_BYTES[i];
        i += 1;
    }
    // payload_len = 0 — empty table.
    let mut j = 0;
    while j < LEN_PREFIX_BYTES {
        b[MAGIC_LEN + j] = 0;
        j += 1;
    }
    b
};

/// Cached table parsed from `SPLICER_CONFIG_BLOB` on the first call;
/// every subsequent `get` reuses the same `&'static HashMap`, so the
/// byte-scan parse runs once per wasm-instance lifetime.
fn table() -> &'static HashMap<String, String> {
    static T: OnceLock<HashMap<String, String>> = OnceLock::new();
    T.get_or_init(parse_table)
}

fn parse_table() -> HashMap<String, String> {
    // `black_box` forces the compiler to treat the static as opaque,
    // so it can't fold the parse against the compile-time initial
    // (empty) blob and drop the runtime lookup.
    let blob: &[u8] = std::hint::black_box(&SPLICER_CONFIG_BLOB);
    // Skip past the magic prefix; peel off the payload-length header;
    // hand the payload bytes to the shared deserializer.
    let body = &blob[MAGIC_LEN..];
    let Some(payload_len) = read_u32_le(body, 0) else {
        return HashMap::new();
    };
    let payload_len = payload_len as usize;
    let payload_end = LEN_PREFIX_BYTES.saturating_add(payload_len);
    if payload_end > body.len() {
        return HashMap::new();
    }
    deserialize_table(&body[LEN_PREFIX_BYTES..payload_end])
}

pub struct ConfigProvider;

impl Guest for ConfigProvider {
    fn get(key: String) -> Option<String> {
        table().get(&key).cloned()
    }
}

bindings::export!(ConfigProvider with_types_in bindings);
