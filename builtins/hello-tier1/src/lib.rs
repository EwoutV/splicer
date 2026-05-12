mod bindings {
    wit_bindgen::generate!({
        world: "hello-tier1-mdl",
        async: true,
        generate_all
    });
}

use std::sync::OnceLock;

use crate::bindings::exports::splicer::tier1::after::Guest as AfterGuest;
use crate::bindings::exports::splicer::tier1::before::Guest as BeforeGuest;
use crate::bindings::splicer::builtin_config::get::get as get_config;
use crate::bindings::splicer::common::types::CallId;

/// Print prefix. Read from the `greeting` config key on first call
/// via `splicer:builtin-config/get` (async — wit-bindgen generates
/// every imported func as async when the world is `async: true`) and
/// cached for the rest of the instance's lifetime. Falls back to
/// `"hello-tier1"` when no `config:` block sets `greeting`.
///
/// Concurrent racers that both find `G` uninitialized will each call
/// `get_config` once and then race on `get_or_init`; the loser's
/// value is dropped and both return the same `&'static str`.
async fn greeting() -> &'static str {
    static G: OnceLock<String> = OnceLock::new();
    if let Some(g) = G.get() {
        return g.as_str();
    }
    let val = get_config("greeting".to_string())
        .await
        .unwrap_or_else(|| "hello-tier1".to_string());
    G.get_or_init(|| val).as_str()
}

pub struct HelloTier1;

impl BeforeGuest for HelloTier1 {
    async fn on_call(call: CallId) {
        let g = greeting().await;
        println!(
            "[{}] before {}#{}",
            g, call.interface_name, call.function_name
        );
    }
}

impl AfterGuest for HelloTier1 {
    async fn on_return(call: CallId) {
        let g = greeting().await;
        println!(
            "[{}] after  {}#{}",
            g, call.interface_name, call.function_name
        );
    }
}

bindings::export!(HelloTier1 with_types_in bindings);
