# sum_tree (Apache-2.0)

Source: https://github.com/zed-industries/zed/tree/8d932b0e0671645500b567cd474cb113085638fd/crates/sum_tree

Upstream commit: `8d932b0e0671645500b567cd474cb113085638fd`.

NotMux patches only this crate to avoid the GPL-licensed `ztracing`,
`ztracing_macro`, and `zlog` dependencies. The original Apache license and
Zed copyright are preserved in `LICENSE-APACHE`.

Local changes:

- Make the manifest independent of the Zed workspace, preserving dependency
  sources, version requirements, features, and the `test-support` feature.
- Remove two `ztracing` imports and seven profiling attributes. These attributes
  are no-ops in normal builds; Zed's separate Tracy profiling mode is not retained.
- Remove the test-only `zlog` initializer and dependencies used only by that
  initializer or the removed profiling instrumentation.

Tree algorithms, public APIs, and all test cases are unchanged.
`src/tree_map.rs` and `src/property_test.rs` are unmodified upstream files.

When updating GPUI, compare this crate against its corresponding upstream
revision before updating or removing the patch. Do not update it independently.
