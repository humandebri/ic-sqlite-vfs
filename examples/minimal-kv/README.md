# minimal-kv

Minimal `ic-sqlite-vfs` canister using a dedicated `MemoryId`.

This example uses `sqlite-precompiled`, so no `.cargo/config.toml` or C compiler
support script is needed.

```sh
cd examples/minimal-kv
dfx deploy
dfx canister call minimal_kv kv_put '("hello", "world")'
dfx canister call minimal_kv kv_get '("hello")'
```

Expected read result:

```text
(variant { Ok = opt "world" })
```

Upgrade persistence check:

```sh
dfx deploy
dfx canister call minimal_kv kv_get '("hello")'
```

If your dfx version supports it, `dfx deploy --upgrade-unchanged` also forces an
upgrade without changing the Wasm.
