# WIP: import EVM state from different dumps

# Import JSON dump into trie

```sh
cargo run -- \
import \
--ledger "/home/maksimv/Desktop/test_ledger" \
--evm-dump "/home/maksimv/Desktop/velas/ropsten-dump-patched-incompletes-tiny.json" \
--state-root "0x6e3b38830bcad692e5121c0f46cc5be7f803439cde7778e732f15010beb40bf3"
```

```sh
cargo run -- \
import \
--ledger "/home/maksimv/Desktop/test_ledger" \
--evm-dump "/home/maksimv/Desktop/velas/ropsten-dump-patched-incompletes-medium.json" \
--state-root "0x5cfa1168c59c32048aeb8c81945bfb645c7370923a419766f10fddb4efb0a129"
```

# Get account data

```sh
cargo run -- \
get \
--ledger "/path/to/empty/dir" \
--evm-dump "/path/to/evm/dump.json"
```
