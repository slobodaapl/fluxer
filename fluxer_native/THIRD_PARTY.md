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

## Arborium syntax highlighting (2.18.2)

The desktop crate enables `arborium` 2.18.2 with its `all-languages` feature
and directly depends on `arborium-highlight` and `arborium-theme` 2.18.2. The
locked graph contains 112 grammar crates and the Arborium support crates
`arborium`, `arborium-highlight`, `arborium-theme`, `arborium-tree-sitter`,
and `arborium-sysroot` (the last is a target-specific WebAssembly dependency).
The Arborium packages identify <https://github.com/bearcove/arborium> as their
repository; the grammar READMEs identify Amos Wenger as Arborium's maintainer.

The published grammar READMEs state that the generated parser code uses each
upstream grammar's license and the Arborium glue uses MIT OR Apache-2.0. Their
repository URLs, recorded upstream revisions, and declared grammar licenses
are listed in [the grammar provenance inventory](LICENSES/arborium-2.18.2/GRAMMARS.md).
Twenty published READMEs have an empty upstream revision field. The package
metadata declares the following grammar licenses:

| License expression | Grammar crates |
| --- | --- |
| MIT | 93 grammars (see inventory) |
| Apache-2.0 | `arborium-asciidoc`, `arborium-elixir`, `arborium-erlang`, `arborium-gleam`, `arborium-hcl`, `arborium-ini`, `arborium-jinja2`, `arborium-query`, `arborium-yuri` |
| MIT OR Apache-2.0 | `arborium-cedar`, `arborium-cedarschema`, `arborium-ron`, `arborium-styx` |
| GPL-3.0 | `arborium-nginx` |
| MPL-2.0 | `arborium-uiua` |
| Apache-2.0 WITH LLVM-exception | `arborium-wit` |
| ISC | `arborium-textproto` |
| Unlicense | `arborium-clojure`, `arborium-fish` |

The `arborium`, `arborium-highlight`, and `arborium-theme` package metadata
declares MIT OR Apache-2.0; `arborium-tree-sitter` and `arborium-sysroot`
declare MIT. The added `tree-sitter-language` 0.1.9 dependency declares MIT;
`streaming-iterator` 0.1.9 declares MIT OR Apache-2.0. The local published
crates include the following notices, retained without alteration:

- [arborium-tree-sitter MIT license](LICENSES/arborium-2.18.2/tree-sitter-MIT.txt)
  and its [Unicode/ICU notices](LICENSES/arborium-2.18.2/tree-sitter-unicode-notices.txt).
- [tree-sitter-language MIT license](LICENSES/arborium-2.18.2/tree-sitter-language-MIT.txt).
- [streaming-iterator MIT](LICENSES/arborium-2.18.2/streaming-iterator-MIT.txt)
  and [Apache-2.0](LICENSES/arborium-2.18.2/streaming-iterator-Apache-2.0.txt)
  alternatives.

**Release packaging gap:** the locally published Arborium grammar archives
and the other Arborium support archives do not contain license or copyright
notice files, despite the license expressions and provenance in their metadata
and READMEs. The inventory above therefore does not supply the individual
upstream copyright notices or complete license texts. Before distributing a
binary, obtain and review the applicable upstream notices and license texts,
including the GPL-3.0, MPL-2.0, and LLVM-exception materials, and verify the
corresponding source and notice obligations for the shipped artifacts. The
native tree does not currently establish that its release packaging includes
this document or the `LICENSES` directory; ensure they accompany the release.
This is an attribution inventory, not a complete license clearance for the
entire dependency graph.
