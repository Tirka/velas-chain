use borsh::BorshSerialize;
use evm_bridge::bridge::EvmBridge;
use evm_rpc::bundler::UserOperation;
use evm_rpc::Bytes;
use solana_evm_loader_program::instructions::FeePayerType;
use solana_evm_loader_program::{
    big_tx_allocate, big_tx_execute, big_tx_write, transfer_native_to_evm_ixs,
};
use solana_sdk::system_instruction;
use {
    log::*,
    solana_rpc::rpc::JsonRpcConfig,
    solana_sdk::{
        signature::{Keypair, Signer},
        transaction::Transaction,
    },
    solana_streamer::socket::SocketAddrSpace,
    solana_test_validator::TestValidatorGenesis,
    std::time::Duration,
};

/// This test checks that simulate_user_op() with valid input reverts with expected result
///
/// What is needed:
/// - account for contract
/// - contract code that reverts (check original contract and compile it if it's simple)
///
#[test]
fn test_test() {
    solana_logger::setup_with("bridge=debug");

    log::info!("Starting test validator...");

    let chain_id = 0xdead;

    let alice = Keypair::new();
    let big_tx_storage = Keypair::new();
    let test_validator = TestValidatorGenesis::default()
        .rpc_config(JsonRpcConfig {
            max_batch_duration: Some(Duration::from_secs(0)),
            ..JsonRpcConfig::default_for_test()
        })
        // .add_account(alice.pubkey(), AccountSharedData::new(100_000_000_000, 100_000, &alice.pubkey()))
        .start_with_mint_address(alice.pubkey(), SocketAddrSpace::Unspecified)
        .expect("validator start failed");
    let rpc_url = test_validator.rpc_url();

    let evm_secret_key = evm_state::SecretKey::from_slice(&[1; 32]).unwrap();
    let evm_address = evm_state::addr_from_public_key(&evm_state::PublicKey::from_secret_key(
        evm_state::SECP256K1,
        &evm_secret_key,
    ));

    let client = test_validator.get_rpc_client();

    log::info!("Funding `alice` with EVM tokens...");
    let _signature = {
        let blockhash = dbg!(client.get_latest_blockhash().unwrap());
        let ixs = transfer_native_to_evm_ixs(alice.pubkey(), 1_000_000_000, evm_address);
        let tx = Transaction::new_signed_with_payer(&ixs, None, &[&alice], blockhash);
        client.send_and_confirm_transaction(&tx).unwrap()
    };

    log::info!("Preparing `EntryPoint.sol` contract...");
    // create transaction with EntryPoint contract
    // // SPDX-License-Identifier: GPL-3.0
    // pragma solidity >=0.6.12 <0.7.0;
    // contract EntryPoint {
    //     function do_nope() public pure returns (uint) {
    //         revert ("nope");
    //     }
    // }
    //let entry_point_contract: &str = "6080604052348015600f57600080fd5b5060d98061001e6000396000f3fe6080604052348015600f57600080fd5b506004361060285760003560e01c8063fbfb6b6c14602d575b600080fd5b60336035565b005b6040517f08c379a00000000000000000000000000000000000000000000000000000000081526004018080602001828103825260048152602001807f6e6f70650000000000000000000000000000000000000000000000000000000081525060200191505060405180910390fdfea2646970667358221220c2cb05baacde8edc2d0a78537d1470b5d95c726528cfddcdcd3a6664c79b207d64736f6c634300060c0033";
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
    log::info!("EntryPoint.sol size: {} bytes. Address: {:?}", tx_bytes.len(), entry_point_address);

    log::info!("Creating storage for EVM Big TX...");
    let _signature = {
        let blockhash = dbg!(client.get_latest_blockhash().unwrap());
        let create_storage_ix = system_instruction::create_account(
            &alice.pubkey(),
            &big_tx_storage.pubkey(),
            100_000_000_000,
            tx_bytes.len() as u64,
            &solana_evm_loader_program::ID,
        );
        let ixs = vec![create_storage_ix];
        let tx = Transaction::new_signed_with_payer(&ixs, None, &[&alice, &big_tx_storage], blockhash);
        client.send_and_confirm_transaction(&tx).unwrap()
    };

    log::info!("Allocating Big TX...");
    let _signature = {
        let blockhash = dbg!(client.get_latest_blockhash().unwrap());
        let allocate = big_tx_allocate(big_tx_storage.pubkey(), tx_bytes.len());
        let ixs = vec![allocate];
        let tx = Transaction::new_signed_with_payer(
            &ixs,
            Some(&alice.pubkey()),
            &[&big_tx_storage, &alice],
            blockhash,
        );
        client.send_and_confirm_transaction(&tx).unwrap()
    };

    const CHUNK_SIZE: usize = 700;
    let chunks = f64::ceil(tx_bytes.len() as f64 / CHUNK_SIZE as f64) as usize;
    log::info!("Writing Big TX in {chunks} chunks...");
    for chunk_idx in 0..chunks {
        let slice_start = chunk_idx * CHUNK_SIZE;
        let slice_end = std::cmp::min((chunk_idx + 1) * CHUNK_SIZE, tx_bytes.len());
        log::info!("Writing Big TX slice[{slice_start}..{slice_end}]...");
        let blockhash = dbg!(client.get_latest_blockhash().unwrap());
        let write_chunk = big_tx_write(
            big_tx_storage.pubkey(),
            slice_start as u64,
            tx_bytes[slice_start..slice_end].to_vec(),
        );
        let ixs = vec![write_chunk];
        let tx = Transaction::new_signed_with_payer(
            &ixs,
            Some(&alice.pubkey()),
            &[&big_tx_storage, &alice],
            blockhash,
        );
        let _signature = client.send_and_confirm_transaction(&tx).unwrap();
    }

    log::info!("Executing Big TX...");
    let _signature = {
        let blockhash = dbg!(client.get_latest_blockhash().unwrap());
        let execute = big_tx_execute(big_tx_storage.pubkey(), None, FeePayerType::Evm);
        let ixs = vec![execute];
        let tx = Transaction::new_signed_with_payer(
            &ixs,
            Some(&alice.pubkey()),
            &[&big_tx_storage, &alice],
            blockhash,
        );
        client.send_and_confirm_transaction(&tx).unwrap()
    };

    log::info!("Validating simulation...");
    let bridge = EvmBridge::new_for_test(chain_id, vec![], rpc_url);
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
    let res = tokio_test::block_on(bridge.get_bundler().simulate_user_op(
        bridge.get_rpc_client(),
        &user_op,
        entry_point_address,
    ))
    .unwrap();
    error!("{:?}", res);

    // assert!(false);
}
