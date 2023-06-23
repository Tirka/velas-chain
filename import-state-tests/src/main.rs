use evm_state::Storage;
use primitive_types::H256;
use solana_sdk::genesis_config::GenesisConfig;
use solana_sdk::genesis_config::evm_genesis::GethAccountExtractor;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(clap::Parser)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(clap::Subcommand)]
enum Commands {
    /// Import account data from dump file into trie
    Import(ImportArgs),

    /// Retrieves data from triedb
    Get(GetArgs),
}

fn main() {
    env_logger::init();

    match <Cli as clap::Parser>::parse().command {
        Commands::Import(args) => command_import(args),
        Commands::Get(args) => command_get(args),
    }
}

#[derive(clap::Args)]
struct ImportArgs {
    /// Path to triedb
    #[clap(long, value_name = "DIR")]
    ledger: String,

    /// Geth' EVM json dump file
    #[clap(long, value_name = "FILE")]
    evm_dump: String,

    /// Optionally verify state root hash of database
    #[clap(long, value_name = "H256", default_value = None)]
    state_root: Option<String>,
}

fn command_import(args: ImportArgs) {
    let ledger_path = Path::new(&args.ledger);
    let evm_state_json = Path::new(&args.evm_dump);
    let genesis_config: GenesisConfig = GenesisConfig {
        evm_root_hash: args.state_root.map(parse_state_root).unwrap(),
        ..Default::default()
    };
    let geth_dump_reader = GethAccountExtractor::open_dump(evm_state_json).unwrap();
    
    log::info!("Begin: {}", now());
    genesis_config
        .generate_evm_state_from_dump(ledger_path, geth_dump_reader)
        .unwrap();
    log::info!("End: {}", now());
}

#[derive(clap::Args)]
struct GetArgs {
    /// Path to triedb
    #[clap(long, value_name = "DIR")]
    ledger: String,

    /// Path to triedb
    #[clap(long, value_name = "H160")]
    key: String,
}

fn command_get(args: GetArgs) {
    let path = Path::new(&args.ledger);
    let storage =
        Storage::open_persistent(path, true).expect("Unable to open ledger at path: {path}");
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn parse_state_root(state_root: String) -> H256 {
    let err_msg = format!("Unable to parse state root: {state_root}");
    let state_root = match state_root.len() {
        66 => hex::decode(&state_root[2..]).expect(&err_msg),
        64 => hex::decode(&state_root[..]).expect(&err_msg),
        _ => panic!("{err_msg}"),
    };
    H256::from_slice(&state_root)
}

// state_root = "5cfa1168c59c32048aeb8c81945bfb645c7370923a419766f10fddb4efb0a129"

// {"balance":"3","nonce":0,"root":"0x56e81f171bcc55a6ff8345e692c0f86e5b48e01b996cadc001622fb5e363b421","codeHash":"0xc5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470","address":"0x00000000000000000000000000000000000000df","key":"0x005e54f1867fd030f90673b8b625ac8f0656e44a88cfc0b3af3e3f3c3d486960"}
// {"balance":"2000010000000701","nonce":0,"root":"0x56e81f171bcc55a6ff8345e692c0f86e5b48e01b996cadc001622fb5e363b421","codeHash":"0xc5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470","address":"0x0000000000000000000000000000000000000088","key":"0x021fe3360ba8c02e194f8e7facdeb9088b3cf433b6498bd6900d50df0266ffe3"}
// {"balance":"1","nonce":0,"root":"0x56e81f171bcc55a6ff8345e692c0f86e5b48e01b996cadc001622fb5e363b421","codeHash":"0xc5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470","address":"0x000000000000000000000000000000000000008e","key":"0x028e62cb4665fce19ae1fc13a604618d7d20be037fc68b63beb3384dfa5ab776"}
// {"balance":"10000000557","nonce":0,"root":"0x56e81f171bcc55a6ff8345e692c0f86e5b48e01b996cadc001622fb5e363b421","codeHash":"0xc5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470","address":"0x0000000000000000000000000000000000000068","key":"0x02cb51767354e1fe6bd4a49b64b3721ffddbc95fed1b8ead005c39bbc07bc4d8"}
