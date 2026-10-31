# GLib 0.18.5 backport

This directory is the published `glib` 0.18.5 crate, retaining its version,
copyright and MIT license. The only upstream source change is in
`src/variant_iter.rs`: the `g_variant_get_child` out-parameter is a mutable
pointer passed by mutable reference.

This backports [gtk-rs/gtk-rs-core#1343](https://github.com/gtk-rs/gtk-rs-core/pull/1343)
to fix [RUSTSEC-2024-0429](https://rustsec.org/advisories/RUSTSEC-2024-0429.html).
Tauri 2's GTK3 dependencies require the 0.18 API, so replacing it with the
registry's 0.20 release would break those types. No version number is changed
to evade advisory checks. Cargo's root `[patch.crates-io]` uses this source on
every platform; Linux CI exercises the affected iterator in optimized code.

Remove this directory and the patch once the upstream Tauri/GTK dependencies
use a fixed registry release. The complete patched source is included in
Fluxlide's source archive for each release.
