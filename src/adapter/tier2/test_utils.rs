//! Shared fixture helpers for tier-2 unit tests.

use wit_parser::{InterfaceId, Resolve};

/// Find an interface by its unversioned qname, ignoring any `@x.y.z` suffix.
pub(super) fn iface_by_unversioned_qname(resolve: &Resolve, qname: &str) -> InterfaceId {
    resolve
        .interfaces
        .iter()
        .find_map(|(id, _)| {
            let q = resolve.id_of(id)?;
            let unversioned = q.split('@').next().unwrap_or(&q);
            (unversioned == qname).then_some(id)
        })
        .unwrap_or_else(|| panic!("interface `{qname}` not found in resolve"))
}
