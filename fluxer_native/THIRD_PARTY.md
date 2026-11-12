# Third-party source attribution

The native text-input implementation in `crates/desktop/src/input.rs` is adapted
from GPUI 0.2.2's `examples/input.rs`, distributed by Zed Industries under the
Apache License, Version 2.0. The adaptation adds Fluxer input behavior, secret
masking, focus/scroll handling, and Unicode/IME corrections. The upstream license
is retained in [LICENSES/Apache-2.0.txt](LICENSES/Apache-2.0.txt).

Source: https://github.com/zed-industries/zed/tree/main/crates/gpui/examples

Other third-party crates remain dependencies governed by their own declared
licenses. Existing Fluxer artwork and source retain the licensing described in
the repository root README and LICENSE.
