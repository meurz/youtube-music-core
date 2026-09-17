# QuickJS runtime notices

The compiled binary uses `rquickjs`, `rquickjs-core` and `rquickjs-sys` 0.11.0
(<https://github.com/DelSkayn/rquickjs>), licensed MIT; see `rquickjs-LICENSE`.
The bundled QuickJS-ng engine declares version 0.11.0 in `quickjs.h`
(<https://github.com/quickjs-ng/quickjs>), licensed MIT; see `quickjs-ng-LICENSE`.

These notices are copied unchanged from the crates.io 0.11.0 distributions.
Cargo.lock pins the Rust crates and their checksums; engine sources are bundled
inside rquickjs-sys, not fetched during normal builds. The Rust integration does
not register the QuickJS libc std/os modules or a module loader.
