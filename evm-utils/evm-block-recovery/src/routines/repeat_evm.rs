use anyhow::{Context, Result};
use evm_state::Block;
use solana_storage_bigtable::LedgerStorage;
use tokio::sync::mpsc;

use crate::routines::BlockMessage;

static CHANNEL_MAX_MESSAGES: usize = 300;

pub async fn repeat_evm(
    block_number: u64,
    limit: u64,
    src: LedgerStorage,
    dst: LedgerStorage,
) -> Result<()> {
    if limit == 1 {
        log::info!("Repeat EVM Block {}", block_number)
    } else {
        log::info!(
            "Repeat EVM Blocks from {} to {}. Total iterations: {}",
            block_number,
            block_number + limit - 1,
            limit
        )
    }

    let (sender, mut receiver) = mpsc::channel::<BlockMessage<Block>>(CHANNEL_MAX_MESSAGES);
    let writer = tokio::spawn(async move {
        log::info!("Writer task started");

        let mut success = vec![];
        let mut error = vec![];

        while let Some(message) = receiver.recv().await {
            let uploaded = dst
                .upload_evm_block(message.block_number, message.block)
                .await
                .context(format!(
                    "Unable to upload block {} to the Destination Ledger",
                    message.block_number
                ));

            match uploaded {
                Ok(()) => {
                    log::info!(
                        "[{}] Block {} uploaded successfully",
                        message.idx,
                        message.block_number
                    );
                    success.push(message.block_number);
                }
                Err(_) => {
                    log::error!(
                        "[{}] Failed to upload block {} to the Destination Ledger",
                        message.idx,
                        message.block_number
                    );
                    error.push(message.block_number);
                }
            }
        }

        log::info!("Writer task ended.");
        log::info!(
            "Successful writes: {}. Erroneous writes: {}",
            success.len(),
            error.len()
        );
        log::warn!("Erroneous block numbers: {:?}", error);
    });

    for (idx, block_number) in (block_number..block_number + limit).enumerate() {
        let idx = idx + 1;

        log::info!(
            "[{}] Reading block {} from the Source Ledger",
            idx,
            block_number
        );

        let block = src
            .get_evm_confirmed_full_block(block_number)
            .await
            .context(format!(
                "Unable to read Evm Block {} from the Source Ledger",
                block_number
            ))?;

        sender
            .send(BlockMessage {
                idx,
                block,
                block_number,
            })
            .await?;
    }

    drop(sender);

    log::info!("Reading complete, awaiting tasks to finish...");

    writer.await.context("Writer job terminated with error")
}
