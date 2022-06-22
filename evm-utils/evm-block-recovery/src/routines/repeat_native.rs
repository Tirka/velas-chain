use anyhow::{bail, Context, Result};
use solana_sdk::{clock::Slot, hash::Hash};
use solana_storage_bigtable::LedgerStorage;
use solana_transaction_status::{
    ConfirmedBlock, ConfirmedBlockWithOptionalMetadata, TransactionWithMetadata,
    TransactionWithOptionalMetadata,
};
use tokio::sync::mpsc;

use crate::routines::BlockMessage;

static CHANNEL_MAX_MESSAGES: usize = 300;
static WRITERS_POOL: usize = 5;

#[derive(Debug, Default)]
struct History {
    oks: Vec<Slot>,
    upload_failures: Vec<(Slot, String)>,
    missing_metas: Vec<(Slot, Hash)>,
}

impl History {
    pub fn collect_ok(&mut self, block_number: Slot) {
        self.oks.push(block_number)
    }

    pub fn collect_upload_failure(&mut self, block_number: Slot, error_msg: String) {
        self.upload_failures.push((block_number, error_msg))
    }

    pub fn collect_missing_meta(&mut self, block_number: Slot, tx_msg_hash: Hash) {
        self.missing_metas.push((block_number, tx_msg_hash))
    }
}

pub async fn repeat_native(
    start_slot: u64,
    end_slot: u64,
    src: LedgerStorage,
    dst: LedgerStorage,
) -> Result<()> {
    if end_slot < start_slot {
        bail!("`end_slot` should be greater or equal than `start_slot`")
    }

    let limit = end_slot as usize - start_slot as usize + 1;

    match limit {
        1 => log::info!("Repeat Native Block {}", start_slot),
        _ => log::info!("Repeat Native Blocks from {} to {}", start_slot, end_slot),
    }

    log::info!("Requesting confirmed blocks: start slot = {start_slot}, limit = {limit}");

    let mut blocks_to_repeat: Vec<u64> = src
        .get_confirmed_blocks(start_slot, limit)
        .await
        .context(format!(
            "Unable to read Native Block {} from the Source Ledger",
            start_slot
        ))?
        .into_iter()
        .filter(|slot| *slot <= end_slot)
        .collect();

    log::info!("Response: {} blocks, trimming...", blocks_to_repeat.len());

    blocks_to_repeat.retain(|x| x <= &end_slot);

    log::info!(
        "Blocks to repeat: start slot = {}, end slot = {}, total = {}",
        blocks_to_repeat[0],
        blocks_to_repeat.last().unwrap(),
        blocks_to_repeat.len()
    );

    let (sender, mut receiver) =
        mpsc::channel::<BlockMessage<ConfirmedBlockWithOptionalMetadata>>(CHANNEL_MAX_MESSAGES);

    let writer = tokio::spawn(async move {
        use futures::StreamExt;

        let receiver = async_stream::stream! {
            while let Some(message) = receiver.recv().await {
                yield (dst.clone(), message);
            }
        };

        receiver.for_each_concurrent(WRITERS_POOL, |(dst, message)| async move {

            let ConfirmedBlockWithOptionalMetadata {
                previous_blockhash,
                blockhash,
                parent_slot,
                transactions,
                rewards,
                block_time,
                block_height,
            } = message.block;

            let transactions = transactions.into_iter().map(|tx| {
                let TransactionWithOptionalMetadata { transaction, meta } = tx;

                let meta = meta.unwrap_or_else(|| {
                    let block_number = message.block_number;
                    let message_hash = transaction.message.hash();

                    log::warn!("Block {block_number} contains transaction with no meta. Message hash = {message_hash}");

                    // history.collect_missing_meta(block_number, message_hash);

                    Default::default()
                });

                TransactionWithMetadata { transaction, meta }
            }).collect::<Vec<_>>();

            let block = ConfirmedBlock {
                previous_blockhash,
                blockhash,
                parent_slot,
                transactions,
                rewards,
                block_time,
                block_height,
            };

            let uploaded = dst
                .upload_confirmed_block(message.block_number, block)
                .await;

            match uploaded {
                Ok(()) => {
                    log::trace!(
                        "[{}] Block {} uploaded successfully",
                        message.idx,
                        message.block_number
                    );
                    // history.collect_ok(message.block_number);
                }
                Err(err) => {
                    log::error!(
                        "[{}] Failed to upload block {} to the Destination Ledger",
                        message.idx,
                        message.block_number
                    );
                    let error_msg = err.to_string();
                    log::trace!("{error_msg}");
                    // history.collect_upload_failure(message.block_number, error_msg);
                }
            }
        })
    });

    for (idx, block_number) in blocks_to_repeat.into_iter().enumerate() {
        let idx = idx + 1;

        log::trace!(
            "[{}] Reading block {} from the Source Ledger",
            idx,
            block_number
        );

        let block = src.get_confirmed_block(block_number).await;

        match block {
            Ok(block) => {
                sender
                    .send(BlockMessage {
                        idx,
                        block,
                        block_number,
                    })
                    .await?;
            }
            Err(err) => {
                log::warn!(
                    "[{}] Unable to read block {} from the Source Ledger",
                    idx,
                    block_number
                );
                log::warn!("{}", err.to_string())
            }
        }
    }

    drop(sender);

    log::info!("Reading complete, awaiting tasks to finish...");

    // let history = writer.await.context("Writer job terminated with error")?;
    let history = History::default(); // FIXME: collect actual history
    let a = writer.await.unwrap();

    log::info!("Successful writes total: {}", history.oks.len());

    match history.missing_metas.len() {
        0 => log::info!("All transactions metas were converted successfully"),
        n => log::warn!(
            "{n} transaction(s) meta(s) were unwrapped with default value: {:?}",
            history.missing_metas
        ),
    };

    match history.upload_failures.len() {
        0 => log::info!("All blocks were copied successfully"),
        n => log::warn!(
            "{n} block(s) were not copied: {:?}",
            history.upload_failures
        ),
    };

    Ok(())
}
