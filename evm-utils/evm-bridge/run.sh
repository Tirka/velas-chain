#!/bin/bash
export RUST_LOG=debug 
cargo run "/home/maksimv/Desktop/velas/velas-keys/testnet-savings.json" https://api.testnet.velas.com 127.0.0.1:8545 111
