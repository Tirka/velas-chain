use std::{
    collections::HashSet,
    ops::Deref,
    sync::{Arc, Mutex},
};

use ::tokio::sync::mpsc;
use evm_rpc::{error::into_native_error, Bytes, Hex, RPCTransaction};
use evm_state::{Address, TransactionAction, H160, H256, U256};
use log::*;
use serde_json::json;
use solana_client::{rpc_config::RpcSendTransactionConfig, rpc_request::RpcRequest};
use solana_evm_loader_program::{
    scope::{evm, solana},
    tx_chunks::TxChunks,
};
use solana_sdk::{
    commitment_config::{CommitmentConfig, CommitmentLevel},
    instruction::AccountMeta,
    message::Message,
    pubkey::Pubkey,
    signer::Signer,
    system_instruction,
};
use txpool::{
    scoring::Choice, NoopListener, Pool, Readiness, Ready, Scoring, ShouldReplace,
    VerifiedTransaction,
};

use crate::{from_client_error, send_and_confirm_transactions, EvmBridge, EvmResult};

struct AlwaysReady;

impl<T> Ready<T> for AlwaysReady {
    fn is_ready(&mut self, _tx: &T) -> Readiness {
        Readiness::Ready
    }
}

pub struct EthPool {
    /// Pool of transactions awaiting to be deployed
    pool: Mutex<Pool<PooledTransaction, MyScoring, NoopListener>>,
}

impl EthPool {
    pub fn new() -> Self {
        Self {
            pool: Mutex::new(Pool::new(NoopListener, MyScoring, Default::default())),
        }
    }

    /// Imports transaction into the pool
    pub fn import(
        &self,
        tx: PooledTransaction,
    ) -> Result<Arc<PooledTransaction>, txpool::Error<H256>> {
        self.pool.lock().unwrap().import(tx, &MyScoring)
    }

    /// Removes transaction from the pool
    pub fn remove(&self, hash: &H256) -> Option<Arc<PooledTransaction>> {
        self.pool.lock().unwrap().remove(hash, false)
    }

    /// Used for a special case when the transaction was replaced at a time when the worker was already processing it
    pub fn remove_by_nonce(&self, sender: &Address, nonce: U256) -> Option<Arc<PooledTransaction>> {
        let hash = {
            self.pool
                .lock()
                .unwrap()
                .pending_from_sender(AlwaysReady, sender, H256::zero())
                .find(|tx| &tx.sender == sender && tx.nonce == nonce)
                .map(|tx| tx.hash)
        };

        hash.map(|hash| self.remove(&hash)).flatten()
    }

    /// Gets reference to the next transaction in queue ready to be deployed
    pub fn pending(&self) -> Option<Arc<PooledTransaction>> {
        self.pool
            .lock()
            .unwrap()
            .pending(AlwaysReady, H256::zero())
            .next()
    }

    /// Returns nonce from transaction pool, on `None` if the pool doesn't contain
    /// any transactions associated with specified sender
    pub fn transaction_count(&self, sender: &Address) -> Option<U256> {
        self.pool
            .lock()
            .unwrap()
            .pending_from_sender(
                |_tx: &PooledTransaction| Readiness::Ready,
                sender,
                H256::zero(),
            )
            .max_by_key(|tx| tx.nonce)
            .map(|tx| tx.nonce + 1)
    }

    /// Gets transaction from the pool by specified hash
    pub fn transaction_by_hash(&self, tx_hash: Hex<H256>) -> Option<Arc<PooledTransaction>> {
        let pool = self.pool.lock().unwrap();
        pool.find(&tx_hash.0)
    }
}

#[derive(Debug)]
pub struct PooledTransaction {
    pub inner: evm::Transaction,
    pub meta_keys: HashSet<Pubkey>,
    sender: Address,
    hash: H256,
    hash_sender: mpsc::Sender<EvmResult<Hex<H256>>>,
}

impl PooledTransaction {
    pub fn new(
        transaction: evm::Transaction,
        meta_keys: HashSet<Pubkey>,
        hash_sender: mpsc::Sender<EvmResult<Hex<H256>>>,
    ) -> Result<Self, evm_state::error::Error> {
        let hash = transaction.tx_id_hash();
        let sender = transaction.caller()?;

        Ok(Self {
            inner: transaction,
            sender,
            hash,
            meta_keys,
            hash_sender,
        })
    }
}

impl VerifiedTransaction for PooledTransaction {
    type Hash = H256;

    type Sender = Address;

    fn hash(&self) -> &Self::Hash {
        &self.hash
    }

    fn mem_usage(&self) -> usize {
        0 // TODO: return correct value
    }

    fn sender(&self) -> &Self::Sender {
        &self.sender
    }
}

impl Deref for PooledTransaction {
    type Target = evm::Transaction;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

#[derive(Debug)]
pub struct MyScoring;

impl Scoring<PooledTransaction> for MyScoring {
    type Score = H256;

    type Event = ();

    fn compare(&self, old: &PooledTransaction, other: &PooledTransaction) -> std::cmp::Ordering {
        old.nonce.cmp(&other.nonce)
    }

    fn choose(&self, old: &PooledTransaction, new: &PooledTransaction) -> Choice {
        if old.nonce == new.nonce {
            if new.gas_price > old.gas_price {
                Choice::ReplaceOld
            } else {
                Choice::RejectNew
            }
        } else {
            Choice::InsertNew
        }
    }

    fn update_scores(
        &self,
        _txs: &[txpool::Transaction<PooledTransaction>],
        _scores: &mut [Self::Score],
        _change: txpool::scoring::Change<Self::Event>,
    ) {
    }
}

impl ShouldReplace<PooledTransaction> for MyScoring {
    fn should_replace(
        &self,
        _old: &txpool::ReplaceTransaction<PooledTransaction>,
        _new: &txpool::ReplaceTransaction<PooledTransaction>,
    ) -> Choice {
        Choice::InsertNew
    }
}

/// This worker checks for new transactions in pool and tries to deploy them
pub async fn worker_deploy(bridge: Arc<EvmBridge>) {
    loop {
        let tx = bridge.pool.pending();

        if let Some(pooled_tx) = tx {
            let hash = pooled_tx.hash;
            let nonce = pooled_tx.nonce;
            let sender = pooled_tx.sender;
            let meta_keys = pooled_tx.meta_keys.clone();
            let tx = (*pooled_tx).clone();
            info!(
                "Pool worker is trying to deploy tx with = {:?} [tx = {:?}]",
                &hash, tx
            );
            match process_tx(bridge.clone(), tx, hash, sender, meta_keys) {
                Ok(hash) => {
                    info!("Tx with hash = {:?} processed successfully", &hash);
                    let _result = pooled_tx.hash_sender.send(Ok(hash)).await;
                }
                // IF (error recoverable ) { /* just stall, skip remove */ }
                // else { stall, do remove }
                Err(e) => {
                    warn!(
                        "Something went wrong in tx processing with hash = {:?}. Error = {:?}",
                        &hash, &e
                    );
                    let _result = pooled_tx.hash_sender.send(Err(e)).await;
                }
            }
            match bridge.pool.remove(&hash) {
                Some(tx) => {
                    info!("Tx with has = {:?} removed from the pool", tx.hash)
                }
                None => {
                    match bridge.pool.remove_by_nonce(&sender, nonce) {
                        Some(dup_tx) => {
                            info!("Tx was replaced during deploy, duplicate tx with hash = {} removed", dup_tx.hash);
                        }
                        None => {
                            warn!("Transaction from the pool dissapeared mysteriously...")
                        }
                    }
                }
            }
            // tx.channel_sender_notify(result: EvmResult<Hex<H256>>)
        } else {
            trace!("pool worker is idling...");
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }
}

fn process_tx(
    bridge: Arc<EvmBridge>,
    tx: evm_state::Transaction,
    hash: H256,
    sender: H160,
    mut meta_keys: HashSet<Pubkey>,
) -> EvmResult<Hex<H256>> {
    let bytes = bincode::serialize(&tx).unwrap();

    let rpc_tx = RPCTransaction::from_transaction(tx.clone().into())?;

    if bridge.simulate {
        // Try simulate transaction execution
        bridge
            .rpc_client
            .send::<Bytes>(RpcRequest::EthCall, json!([rpc_tx, "latest"]))
            .map_err(from_client_error)?;
    }

    if bytes.len() > evm::TX_MTU {
        debug!("Sending tx = {}, by chunks", hash);
        match deploy_big_tx(&bridge, &bridge.key, &tx) {
            Ok(_tx) => {
                return Ok(Hex(hash));
            }
            Err(e) => {
                error!("Error creating big tx = {}", e);
                return Err(e);
            }
        }
    }

    debug!(
        "Printing tx_info from = {:?}, to = {:?}, nonce = {}, chain_id = {:?}",
        sender,
        tx.address(),
        tx.nonce,
        tx.signature.chain_id()
    );

    // Shortcut for swap tokens to native, will add solana account to transaction.
    if let TransactionAction::Call(addr) = tx.action {
        use solana_evm_loader_program::precompiles::*;

        if addr == *ETH_TO_VLX_ADDR {
            debug!("Found transferToNative transaction");
            match ETH_TO_VLX_CODE.parse_abi(&tx.input) {
                Ok(pk) => {
                    info!("Adding account to meta = {}", pk);
                    meta_keys.insert(pk);
                }
                Err(e) => {
                    error!("Error in parsing abi = {}", e);
                }
            }
        }
    }

    let mut ix =
        solana_evm_loader_program::send_raw_tx(bridge.key.pubkey(), tx, Some(bridge.key.pubkey()));

    // Add meta accounts as additional arguments
    for account in meta_keys {
        ix.accounts.push(AccountMeta::new(account, false))
    }

    let message = Message::new(&[ix], Some(&bridge.key.pubkey()));
    let mut send_raw_tx: solana::Transaction = solana::Transaction::new_unsigned(message);

    debug!("Getting block hash");
    let (blockhash, _fee_calculator, _) = bridge
        .rpc_client
        .get_recent_blockhash_with_commitment(CommitmentConfig::processed())
        .map(|response| response.value)
        // NOTE: into_native_error?
        .map_err(|e| evm_rpc::Error::NativeRpcError {
            details: String::from("Failed to get recent blockhash"),
            source: e.into(),
            verbose: bridge.verbose_errors,
        })?;

    send_raw_tx.sign(&[&bridge.key], blockhash);
    debug!("Sending tx = {:?}", send_raw_tx);

    bridge
        .rpc_client
        .send_transaction_with_config(
            &send_raw_tx,
            RpcSendTransactionConfig {
                preflight_commitment: Some(CommitmentLevel::Processed),
                skip_preflight: !bridge.simulate,
                ..Default::default()
            },
        )
        .map(|_| Hex(hash))
        .map_err(from_client_error)
}

fn deploy_big_tx(
    bridge: &EvmBridge,
    payer: &solana_sdk::signature::Keypair,
    tx: &evm::Transaction,
) -> EvmResult<()> {
    let payer_pubkey = payer.pubkey();

    let storage = solana_sdk::signature::Keypair::new();
    let storage_pubkey = storage.pubkey();

    let signers = [payer, &storage];

    debug!("Create new storage {} for EVM tx {:?}", storage_pubkey, tx);

    let tx_bytes =
        bincode::serialize(&tx).map_err(|e| into_native_error(e, bridge.verbose_errors))?;

    debug!(
        "Storage {} : tx bytes size = {}, chunks crc = {:#x}",
        storage_pubkey,
        tx_bytes.len(),
        TxChunks::new(tx_bytes.as_slice()).crc(),
    );

    let balance = bridge
        .rpc_client
        .get_minimum_balance_for_rent_exemption(tx_bytes.len())
        .map_err(|e| into_native_error(e, bridge.verbose_errors))?;

    let (blockhash, _, _) = bridge
        .rpc_client
        .get_recent_blockhash_with_commitment(CommitmentConfig::finalized())
        .map_err(|e| into_native_error(e, bridge.verbose_errors))?
        .value;

    let create_storage_ix = system_instruction::create_account(
        &payer_pubkey,
        &storage_pubkey,
        balance,
        tx_bytes.len() as u64,
        &solana_evm_loader_program::ID,
    );

    let allocate_storage_ix =
        solana_evm_loader_program::big_tx_allocate(&storage_pubkey, tx_bytes.len());

    let create_and_allocate_tx = solana::Transaction::new_signed_with_payer(
        &[create_storage_ix, allocate_storage_ix],
        Some(&payer_pubkey),
        &signers,
        blockhash,
    );

    debug!(
        "Create and allocate tx signatures = {:?}",
        create_and_allocate_tx.signatures
    );

    bridge
        .rpc_client
        .send_and_confirm_transaction(&create_and_allocate_tx)
        .map(|signature| {
            debug!(
                "Create and allocate {} tx was done, signature = {:?}",
                storage_pubkey, signature
            )
        })
        .map_err(|e| {
            error!("Error create and allocate {} tx: {:?}", storage_pubkey, e);
            into_native_error(e, bridge.verbose_errors)
        })?;

    let (blockhash, _) = bridge
        .rpc_client
        .get_new_blockhash(&blockhash)
        .map_err(|e| into_native_error(e, bridge.verbose_errors))?;

    let write_data_txs: Vec<solana::Transaction> = tx_bytes
        // TODO: encapsulate
        .chunks(evm_state::TX_MTU)
        .enumerate()
        .map(|(i, chunk)| {
            solana_evm_loader_program::big_tx_write(
                &storage_pubkey,
                (i * evm_state::TX_MTU) as u64,
                chunk.to_vec(),
            )
        })
        .map(|instruction| {
            solana::Transaction::new_signed_with_payer(
                &[instruction],
                Some(&payer_pubkey),
                &signers,
                blockhash,
            )
        })
        .collect();

    debug!("Write data txs: {:?}", write_data_txs);

    send_and_confirm_transactions(&bridge.rpc_client, write_data_txs, &signers)
        .map(|_| debug!("All write txs for storage {} was done", storage_pubkey))
        .map_err(|e| {
            error!("Error on write data to storage {}: {:?}", storage_pubkey, e);
            into_native_error(e, bridge.verbose_errors)
        })?;

    let (blockhash, _, _) = bridge
        .rpc_client
        .get_recent_blockhash_with_commitment(CommitmentConfig::processed())
        .map_err(|e| into_native_error(e, bridge.verbose_errors))?
        .value;

    let execute_tx = solana::Transaction::new_signed_with_payer(
        &[solana_evm_loader_program::big_tx_execute(
            &storage_pubkey,
            Some(&payer_pubkey),
        )],
        Some(&payer_pubkey),
        &signers,
        blockhash,
    );

    debug!("Execute EVM transaction at storage {} ...", storage_pubkey);

    let rpc_send_cfg = RpcSendTransactionConfig {
        skip_preflight: false,
        preflight_commitment: Some(CommitmentLevel::Processed),
        ..Default::default()
    };

    bridge
        .rpc_client
        .send_transaction_with_config(&execute_tx, rpc_send_cfg)
        .map(|signature| {
            debug!(
                "Execute EVM tx at {} was done, signature = {:?}",
                storage_pubkey, signature
            )
        })
        .map_err(|e| {
            error!("Execute EVM tx at {} failed: {:?}", storage_pubkey, e);
            from_client_error(e)
        })?;

    // TODO: here we can transfer back lamports and delete storage

    Ok(())
}

#[cfg(test)]
mod tests {
    use txpool::Ready;

    type Pool = txpool::Pool<PooledTransaction, MyScoring, NoopListener>;

    use super::*;

    static SK1: [u8; 32] = [1u8; 32];
    static SK2: [u8; 32] = [2u8; 32];

    #[test]
    fn test_pending_queuing() {
        let mut pool = Pool::new(NoopListener, MyScoring, Default::default());

        import(&mut pool, test_tx(100, 1000, "foo", &SK1));
        import(&mut pool, test_tx(100, 1600, "foo", &SK1));

        assert_eq!(pool.light_status().transaction_count, 1);

        assert_eq!(
            pool.pending(AlwaysReady, H256::zero())
                .next()
                .unwrap()
                .gas_price,
            1600.into()
        );

        import(&mut pool, test_tx(10, 1010, "bar1", &SK2));
        import(&mut pool, test_tx(12, 1012, "last_one", &SK2));
        import(&mut pool, test_tx(11, 1011, "bar2", &SK2));

        assert_eq!(
            pending_msgs(&pool, AlwaysReady),
            vec!["foo", "bar1", "bar2", "last_one"]
        );
    }

    #[test]
    fn test_readiness() {
        let mut pool = Pool::new(NoopListener, MyScoring, Default::default());

        import(&mut pool, test_tx(1, 1, "11", &SK1));
        import(&mut pool, test_tx(1, 100, "22", &SK2));
        import(&mut pool, test_tx(2, 100, "33", &SK1));
        import(&mut pool, test_tx(2, 1, "44", &SK2));

        fn only_high_price(tx: &PooledTransaction) -> Readiness {
            if tx.gas_price > 1.into() {
                Readiness::Ready
            } else {
                Readiness::Stale
            }
        }

        assert_eq!(pending_msgs(&pool, only_high_price), vec!["22", "33"])
    }

    #[test]
    fn test_removing_replaced_transaction() {
        let pool = EthPool::new();

        pool.import(test_tx(1, 100, "11", &SK1)).unwrap();

        assert_eq!(
            pool.pool.lock().unwrap().light_status().transaction_count,
            1
        );

        let next = pool.pending().unwrap();

        pool.import(test_tx(1, 9000, "11", &SK1)).unwrap();

        assert_eq!(
            pool.pool.lock().unwrap().light_status().transaction_count,
            1
        );
        assert!(pool.remove(&next.hash).is_none());

        let removed = pool.remove_by_nonce(&next.sender, next.nonce);

        assert!(removed.is_some());
        assert_eq!(
            pool.pool.lock().unwrap().light_status().transaction_count,
            0
        );
    }

    fn test_tx(nonce: u32, gas_price: u32, msg: &str, secret_key: &[u8; 32]) -> PooledTransaction {
        let tx_create = evm::UnsignedTransaction {
            nonce: nonce.into(),
            gas_price: gas_price.into(),
            gas_limit: 30000000.into(),
            action: evm::TransactionAction::Create,
            value: 0.into(),
            input: msg.as_bytes().to_vec(),
        };

        let secret_key: evm_state::SecretKey = evm::SecretKey::from_slice(secret_key).unwrap();

        let (tx, _) = mpsc::channel(1);
        PooledTransaction::new(tx_create.sign(&secret_key, Some(111)), HashSet::new(), tx).unwrap()
    }

    fn import(pool: &mut Pool, tx: PooledTransaction) {
        pool.import(tx, &mut MyScoring).unwrap();
    }

    fn pending_msgs<R>(pool: &Pool, ready: R) -> Vec<String>
    where
        R: Ready<PooledTransaction>,
    {
        pool.pending(ready, H256::zero())
            .map(|tx| String::from_utf8(tx.input.clone()).unwrap())
            .collect()
    }
}
