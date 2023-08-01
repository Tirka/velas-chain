use borsh::BorshSerialize;
use evm_rpc::bundler::UserOperation;
use evm_rpc::Bytes;
use evm_state::Address;
use solana_evm_loader_program::instructions::FeePayerType;
use solana_evm_loader_program::{
    big_tx_allocate, big_tx_execute, big_tx_write, send_raw_tx, transfer_native_to_evm_ixs,
};
use solana_sdk::account::{AccountSharedData, WritableAccount};
use solana_sdk::{bs58, system_instruction};
use {
    bincode::serialize,
    evm_bridge::bridge::EvmBridge,
    evm_bridge::pool::{EthPool, SystemClock},
    evm_rpc::{BlockId, Hex, RPCLogFilter, RPCTransaction},
    evm_state::TransactionInReceipt,
    log::*,
    primitive_types::{H256, U256},
    reqwest::{self, header::CONTENT_TYPE},
    serde_json::{json, Value},
    solana_account_decoder::UiAccount,
    solana_client::{
        client_error::{ClientErrorKind, Result as ClientResult},
        pubsub_client::PubsubClient,
        rpc_client::RpcClient,
        rpc_config::{RpcAccountInfoConfig, RpcSendTransactionConfig, RpcSignatureSubscribeConfig},
        rpc_request::RpcError,
        rpc_response::{Response as RpcResponse, RpcSignatureResult, SlotUpdate},
        tpu_client::{TpuClient, TpuClientConfig},
    },
    solana_rpc::rpc::JsonRpcConfig,
    solana_sdk::{
        commitment_config::{CommitmentConfig, CommitmentLevel},
        fee_calculator::FeeRateGovernor,
        hash::Hash,
        pubkey::Pubkey,
        rent::Rent,
        signature::{Keypair, Signer},
        system_instruction::assign,
        system_transaction,
        transaction::Transaction,
    },
    solana_streamer::socket::SocketAddrSpace,
    solana_test_validator::{TestValidator, TestValidatorGenesis},
    solana_transaction_status::TransactionStatus,
    std::{
        collections::HashSet,
        net::UdpSocket,
        str::FromStr,
        sync::{mpsc::channel, Arc},
        thread::sleep,
        time::{Duration, Instant},
    },
    tokio::runtime::Runtime,
};

macro_rules! json_req {
    ($method: expr, $params: expr) => {{
        json!({
           "jsonrpc": "2.0",
           "id": 1,
           "method": $method,
           "params": $params,
        })
    }}
}

fn post_rpc(request: Value, rpc_url: &str) -> Value {
    let client = reqwest::blocking::Client::new();
    let response = client
        .post(rpc_url)
        .header(CONTENT_TYPE, "application/json")
        .body(request.to_string())
        .send()
        .unwrap();
    serde_json::from_str(&response.text().unwrap()).unwrap()
}

fn get_blockhash(rpc_url: &str) -> Hash {
    let req = json_req!(
        "getRecentBlockhash",
        json!([json!(CommitmentConfig {
            commitment: CommitmentLevel::Finalized
        })])
    );
    let json = post_rpc(req, &rpc_url);
    json["result"]["value"]["blockhash"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}

fn wait_finalization(rpc_url: &str, signatures: &[&Value]) -> bool {
    let request = json_req!("getSignatureStatuses", [signatures]);

    for _ in 0..solana_sdk::clock::DEFAULT_TICKS_PER_SLOT {
        let json = post_rpc(request.clone(), &rpc_url);
        let values = json["result"]["value"].as_array().unwrap();
        if values.iter().all(|v| !v.is_null()) {
            if values.iter().all(|v| {
                assert_eq!(v["err"], Value::Null);
                v["confirmationStatus"].as_str().unwrap() == "finalized"
            }) {
                warn!("All signatures confirmed: {:?}", dbg!(values));
                return true;
            }
        }

        sleep(Duration::from_secs(1));
    }
    false
}

/// This test checks that simulate_user_op() with valid input reverts with expected result
///
/// What is needed:
/// - account for contract
/// - contract code that reverts (check original contract and compile it if it's simple)
///
#[test]
fn test_test() {
    solana_logger::setup_with("warn");

    let chain_id = 0xdead;

    let alice = Keypair::new();

    // create and run test validator
    let test_validator = TestValidatorGenesis::default()
        .rpc_config(JsonRpcConfig {
            max_batch_duration: Some(Duration::from_secs(0)),
            ..JsonRpcConfig::default_for_test()
        })
        .start_with_mint_address(alice.pubkey(), SocketAddrSpace::Unspecified)
        .expect("validator start failed");
    let rpc_url = test_validator.rpc_url();

    // create evm keypair
    let evm_secret_key = evm_state::SecretKey::from_slice(&[1; 32]).unwrap();
    let evm_address = evm_state::addr_from_public_key(&evm_state::PublicKey::from_secret_key(
        evm_state::SECP256K1,
        &evm_secret_key,
    ));

    // fund EVM address with some tokens
    let blockhash = dbg!(get_blockhash(&rpc_url));
    let ixs = transfer_native_to_evm_ixs(alice.pubkey(), 1000000, evm_address);
    let tx = Transaction::new_signed_with_payer(&ixs, None, &[&alice], blockhash);
    let serialized_encoded_tx = bs58::encode(serialize(&tx).unwrap()).into_string();

    let req = json_req!("sendTransaction", json!([serialized_encoded_tx]));
    let json: Value = post_rpc(req, &rpc_url);
    wait_finalization(&rpc_url, &[&json["result"]]);

    // create transaction with EntryPoint contract
    // // SPDX-License-Identifier: GPL-3.0
    // pragma solidity >=0.6.12 <0.7.0;
    // contract EntryPoint {
    //     function do_nope() public pure returns (uint) {
    //         revert ("nope");
    //     }
    // }
    // let entry_point_contract: &str = "6080604052348015600f57600080fd5b5060d98061001e6000396000f3fe6080604052348015600f57600080fd5b506004361060285760003560e01c8063fbfb6b6c14602d575b600080fd5b60336035565b005b6040517f08c379a00000000000000000000000000000000000000000000000000000000081526004018080602001828103825260048152602001807f6e6f70650000000000000000000000000000000000000000000000000000000081525060200191505060405180910390fdfea2646970667358221220c2cb05baacde8edc2d0a78537d1470b5d95c726528cfddcdcd3a6664c79b207d64736f6c634300060c0033";

    let entry_point_contract = include_str!("./entrypoint.bin").trim();

    let tx_create = evm_state::UnsignedTransaction {
        nonce: 0.into(),
        gas_price: 2000000000.into(),
        gas_limit: 300000.into(),
        action: evm_state::TransactionAction::Create,
        value: 0.into(),
        input: hex::decode(entry_point_contract).unwrap(),
    }
    .sign(&evm_secret_key, Some(chain_id));
    let entry_point_address = tx_create.address().unwrap();

    let mut tx_bytes = vec![];
    BorshSerialize::serialize(&tx_create, &mut tx_bytes).unwrap();

    // crate big tx storage for EntryPoint contract
    let big_tx_storage = Keypair::new();
    let blockhash = dbg!(get_blockhash(&rpc_url));
    let create_storage_ix = system_instruction::create_account(
        &alice.pubkey(),
        &big_tx_storage.pubkey(),
        100_000_000_000,
        tx_bytes.len() as u64,
        &solana_evm_loader_program::ID,
    );
    let ixs = vec![create_storage_ix];
    let tx = Transaction::new_signed_with_payer(&ixs, None, &[&alice, &big_tx_storage], blockhash);
    let serialized_encoded_tx = bs58::encode(serialize(&tx).unwrap()).into_string();
    let req = json_req!("sendTransaction", json!([serialized_encoded_tx]));
    let json = dbg!(post_rpc(req, &rpc_url));
    wait_finalization(&rpc_url, &[&json["result"]]);

    let blockhash = dbg!(get_blockhash(&rpc_url));
    let allocate = big_tx_allocate(big_tx_storage.pubkey(), dbg!(tx_bytes.len()));
    let ixs = vec![allocate];
    let tx = Transaction::new_signed_with_payer(
        &ixs,
        Some(&alice.pubkey()),
        &[&big_tx_storage, &alice],
        blockhash,
    );
    let serialized_encoded_tx = bs58::encode(serialize(&tx).unwrap()).into_string();
    let req = json_req!("sendTransaction", json!([serialized_encoded_tx]));
    let json = dbg!(post_rpc(req, &rpc_url));
    wait_finalization(&rpc_url, &[&json["result"]]);

    const DEPLOY_CHUNK: usize = 700;
    for i in 0..tx_bytes.len() / DEPLOY_CHUNK {
        let slice_start = i * DEPLOY_CHUNK;
        let slice_end = std::cmp::min((i + 1) * DEPLOY_CHUNK, tx_bytes.len());
        let blockhash = dbg!(get_blockhash(&rpc_url));
        let write1 = big_tx_write(
            big_tx_storage.pubkey(),
            0,
            tx_bytes[slice_start..slice_end].to_vec(),
        );
        let ixs = vec![write1];
        let tx = Transaction::new_signed_with_payer(
            &ixs,
            Some(&alice.pubkey()),
            &[&big_tx_storage, &alice],
            blockhash,
        );
        let serialized_encoded_tx = bs58::encode(serialize(&tx).unwrap()).into_string();
        let req = json_req!("sendTransaction", json!([serialized_encoded_tx]));
        let json = dbg!(post_rpc(req, &rpc_url));
        wait_finalization(&rpc_url, &[&json["result"]]);
    }

    let blockhash = dbg!(get_blockhash(&rpc_url));
    let execute = big_tx_execute(big_tx_storage.pubkey(), None, FeePayerType::Evm);
    let ixs = vec![execute];
    let tx = Transaction::new_signed_with_payer(
        &ixs,
        Some(&alice.pubkey()),
        &[&big_tx_storage, &alice],
        blockhash,
    );
    let serialized_encoded_tx = bs58::encode(serialize(&tx).unwrap()).into_string();
    let req = json_req!("sendTransaction", json!([serialized_encoded_tx]));
    let json = dbg!(post_rpc(req, &rpc_url));
    wait_finalization(&rpc_url, &[&json["result"]]);

    let blockhash = dbg!(get_blockhash(&rpc_url));
    let allocate = big_tx_allocate(big_tx_storage.pubkey(), dbg!(tx_bytes.len()));
    let write1 = big_tx_write(big_tx_storage.pubkey(), 0, tx_bytes);
    let execute = big_tx_execute(big_tx_storage.pubkey(), None, FeePayerType::Evm);
    let ixs = vec![allocate, write1, execute];
    let tx = Transaction::new_signed_with_payer(
        &ixs,
        Some(&alice.pubkey()),
        &[&big_tx_storage, &alice],
        blockhash,
    );
    let serialized_encoded_tx = bs58::encode(serialize(&tx).unwrap()).into_string();
    let req = json_req!("sendTransaction", json!([serialized_encoded_tx]));
    let json = dbg!(post_rpc(req, &rpc_url));
    wait_finalization(&rpc_url, &[&json["result"]]);

    let mut bridge = EvmBridge::new_for_test(chain_id, vec![], rpc_url);
    let user_op = UserOperation {
        sender: Default::default(),
        nonce: Default::default(),
        init_code: Bytes::from(Vec::new()),
        call_data: Bytes::from(Vec::new()),
        call_gas_limit: Default::default(),
        verification_gas_limit: Default::default(),
        pre_verification_gas: Default::default(),
        max_fee_per_gas: Default::default(),
        max_priority_fee_per_gas: Default::default(),
        paymaster_and_data: Bytes::from(Vec::new()),
        signature: Bytes::from(Vec::new()),
    };
    //NOTE - Simulate validation
    let res = tokio_test::block_on(bridge.get_bundler().simulate_user_op(
        bridge.get_rpc_client(),
        &user_op,
        entry_point_address,
    ))
    .unwrap();
    error!("{:?}", res);

    // assert!(false);
}
