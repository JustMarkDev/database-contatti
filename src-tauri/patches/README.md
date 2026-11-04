# Patched crates

## `glib` 0.18.5

Copy of the published `glib` 0.18.5 crate with the fix for
[RUSTSEC-2024-0429](https://rustsec.org/advisories/RUSTSEC-2024-0429) /
[GHSA-wrw7-89jp-8q8g](https://github.com/advisories/GHSA-wrw7-89jp-8q8g)
(`glib::VariantStrIter` passed `&p` instead of `&mut p` to an out-argument),
applied in `patches/glib/src/variant_iter.rs`. Upstream fixed this in `glib`
0.20 (gtk-rs-core#1343) but never backported it to 0.18.

Tauri's Linux GTK3 stack (`gtk` 0.18 -> `glib` ^0.18) cannot move to 0.20, so the
crate is wired in with `[patch.crates-io]` in `Cargo.toml`.

Remove this directory and the `[patch.crates-io]` entry once Tauri/tao/wry move
off the gtk3-rs 0.18 bindings.
