# pmux local rmux-client patch

This directory is the complete `rmux-client` 0.9.0 crate published on
crates.io, whose package checksum is
`0229231128141add0463cd755b03ce29e3057086555f893cbe52d36705aefe3f`.
Its `.cargo_vcs_info.json` binds the source to upstream commit
`b2f80522bae2927e22d81e5c902b727623f934d0` in
<https://github.com/Helvesec/rmux> (the peeled `v0.9.0` tag).

pmux changes exactly one upstream source line in `src/attach.rs`. The Unix
attach fast path formerly passed `&read_buffer[consumed..]` to
`decode_attach_data_frame`, exposing the unused remainder of its 8 KiB scratch
buffer. A fragmented read could therefore be decoded against zero or stale
bytes beyond `bytes_read`, causing the decoder to consume bytes that had never
arrived and to treat the true frame length or payload as a later message tag.
The local patch bounds that slice to `&read_buffer[consumed..bytes_read]`.

The authoritative regressions are the direct and managed cases in
`crates/rmux/tests/attach_fragmentation.rs`. They exercise legal
`1 | 4 | payload` fragmentation through the patched public rmux-client entry
points, including `attach_with_terminal`. The offline
`crates/rmux/tests/vendor_patch.rs` gate binds the Cargo graph, the published
archive manifest, every unchanged file, and this exact reversible source-line
replacement. Because this crate is intentionally excluded from the root Cargo
workspace, Gate A also runs its own `Cargo.lock` through standalone Rust 1.88
format, check, strict Clippy, rustdoc, and all-target/all-feature test commands.
This patch must be removed in favor of an upstream release only after the same
regressions pass against that exact replacement.

The upstream license declaration remains `MIT OR Apache-2.0`; the corresponding
license texts are retained at the workspace root as `LICENSE-MIT` and
`LICENSE-APACHE`.
