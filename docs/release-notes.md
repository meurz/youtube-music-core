Version 0.10.0 replaces duplicated catalog and player extraction code with a
pinned RustyPipe backend while retaining JSON protocol 2.0 and C ABI 2.

- Reuse RustyPipe for music search, suggestions, playlists, albums, artists,
  radio, lyrics, supported library categories and Web player URL transforms.
- Remove the previous embedded JavaScript AST parser and custom decipher module;
  player analysis now uses RustyPipe's Rust OXC parser and bounded QuickJS runtime.
- Preserve desktop action metadata, duplicate playlist entry IDs, true pagination,
  account selection, cookie maintenance, cancellation and stream-cache behavior.
- Keep extensions absent from upstream: discovery feeds, saved library artists,
  contextual queues, timed lyrics, writes, DASH adaptation and native SABR delivery.
- Restrict playback to Web Music. No Android/TV fallback, external token server or
  Node process is introduced by the core. PO generation remains a host concern;
  this release does not promise to remove a desktop host's existing PO provider.
- Distribute the combined core under GPL-3.0, retaining original MIT notices.
  Releases include corresponding source with locked dependency sources.

The RustyPipe revision and compatibility patches are documented in
`vendor/rustypipe/UPSTREAM.md`. Personal-account acceptance and Windows ARM64
playback require verification on an available signed-in/native environment.
Google can still reject a stream without a valid host-supplied PO token.
