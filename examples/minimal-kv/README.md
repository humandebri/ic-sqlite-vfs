# minimal-kv

Minimal `ic-sqlite-vfs` canister using a dedicated `MemoryId`.

```sh
scripts/install-build-support.sh examples/minimal-kv
cd examples/minimal-kv
dfx deploy
dfx canister call minimal_kv kv_put '("hello", "world")'
dfx canister call minimal_kv kv_get '("hello")'
```
