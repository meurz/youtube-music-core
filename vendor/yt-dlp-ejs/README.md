# Embedded YouTube player solver

These files are unchanged from the `yt-dlp-ejs` 0.8.0 release:

- Upstream: <https://github.com/yt-dlp/ejs>
- Distribution: <https://pypi.org/project/yt-dlp-ejs/0.8.0/>
- Wheel: `yt_dlp_ejs-0.8.0-py3-none-any.whl`
- Wheel SHA-256: `79300e5fca7f937a1eeede11f0456862c1b41107ce1d726871e0207424f4bdb4`
- `core.min.js` SHA-256: `18da6ce0758b416e7ae645084f4f8801f9f9d59d6c477c05eaa0ff94ebd8cc00`
- `lib.min.js` SHA-256: `c55987fe697e5b9ee18830163f7af85327e9bb5c3e674b969d38c8d205eaa577`

The solver is Unlicense (`LICENSE`). The library bundle contains Meriyah 6.1.4
(ISC) and Astring 1.9.0 (MIT); their complete notices are preserved at the top of
`lib.min.js`. Preserve those notices in redistributed source and binary bundles.

Rust includes these files at compile time. QuickJS runs them in process, with
memory, stack, input and execution limits. No external JavaScript runtime or
Python package is needed at runtime or during a normal Cargo build. The JavaScript
context has no filesystem, process, module loader, network or Rust callbacks.

The adapter uses Astring's supported streaming writer to avoid quadratic string
concatenation in QuickJS. A process-local cache holds at most two prepared player
scripts (16 MiB each), keyed by source SHA-256 and length. Preparation receives no
challenge values or credentials; nothing from this cache is persisted to disk.
Each interpreter stage is limited to 256 MiB heap and 30 seconds, with a 512 KiB
JavaScript stack on a dedicated 4 MiB worker stack.

To update: download an explicit release wheel, verify its PyPI SHA-256, extract
`yt_dlp_ejs/yt/solver/{core,lib}.min.js` and the distribution license, update this
document, and rerun fixture and live player verification. Do not silently fetch
or execute a replacement solver at runtime.
