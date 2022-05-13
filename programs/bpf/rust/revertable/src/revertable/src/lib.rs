use solana_program::{
    account_info::{AccountInfo, next_account_info},
    entrypoint,
    entrypoint::ProgramResult,
    pubkey::Pubkey,
    program_error::ProgramError,
    program::invoke_signed,
    msg
};

use solana_evm_loader_program::{transfer_native_to_evm_ixs};

use primitive_types::H160;
use solana_sdk::transaction::Transaction;

entrypoint!(program);

pub fn program(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    _instruction_data: &[u8],
) -> ProgramResult {
    let owner = Pubkey::new_from_array([7; 32]);
    let lamports = 10_000;
    let ether_address = H160([4; 20]);

    let accs = accounts.iter().collect::<Vec<_>>();
    msg!("accs: {:?}", &accs);
    
    let root = next_account_info(&mut accounts.iter())?;

    let instructions = transfer_native_to_evm_ixs(owner, lamports, ether_address);

    // msg!("✅ instruction executed successfully: {:?}", &i)
    // msg!("❎ instruction executed with error: {}", &program_error)

    // invoke_signed(&instructions[0], accounts, &[&[b"seeds"], &[b"seeds"]]);
    Err(ProgramError::Custom(33))
}
