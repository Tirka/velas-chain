const DEFAULT_INSTANCE: &str = "solana-ledger";
const DEFAULT_BIGTABLE_LIMIT: &str = "150000";

#[derive(clap::Parser)]
pub struct Cli {
    #[clap(subcommand)]
    pub subcommand: Command,

    /// Optionally override "GOOGLE_APPLICATION_CREDENTIALS" environment variable value
    #[clap(long, value_name = "FILE_PATH", value_hint = clap::ValueHint::FilePath)]
    pub creds: Option<String>,

    /// Bigtable Instance
    #[clap(long, value_name = "STRING", default_value = DEFAULT_INSTANCE)]
    pub instance: String,

    /// Enables additional structured output to stdout for use in embedded environment
    #[clap(long, value_name = "BOOL")]
    pub embed: bool,
}

#[derive(clap::Subcommand)]
pub enum Command {
    /// Finds missing ranges of EVM Blocks
    FindEvm(FindEvmArgs),

    /// Finds missing ranges of Native Blocks
    FindNative(FindNativeArgs),

    /// Restores EVM subchain
    RestoreChain(RestoreChainArgs),

    /// Checks content of Native Block
    CheckNative(CheckNativeArgs),

    /// Checks content of Evm Block
    CheckEvm(CheckEvmArgs),

    /// Compares difference of Native Block sets
    CompareNative(CompareNativeArgs),

    /// Uploads blocks to Bigtable from .json file
    Upload(UploadArgs),

    /// Copies sequence of EVM Blocks from Source to Destination Ledger
    RepeatEvm(RepeatEvmArgs),

    /// Copies sequence of Native Blocks from Source to Destination Ledger
    RepeatNative(RepeatNativeArgs),

    /// Generetes Shell Completions for this Utility
    Completion(CompletionArgs),
}

#[derive(clap::Args)]
pub struct FindEvmArgs {
    /// Starting EVM Block number
    #[clap(long, value_name = "NUM")]
    pub start_block: u64,

    /// Limit of EVM Blocks to search
    #[clap(long, value_name = "NUM")]
    pub end_block: Option<u64>,

    #[clap(long, value_name = "NUM")]
    /// Alternative to `end_block` if it's not set
    pub limit: Option<u64>,

    /// Bigtable limit TODO: implement bitgable limit for evm part
    #[clap(long, value_name = "NUM", default_value = DEFAULT_BIGTABLE_LIMIT)]
    pub bigtable_limit: usize, // TODO: max_chunk_size
}

#[derive(clap::Args)]
pub struct FindNativeArgs {
    /// Starting Native Block number
    #[clap(long, value_name = "NUM")]
    pub start_block: u64,

    /// Last Native Block to search
    #[clap(long, value_name = "NUM")]
    pub end_block: Option<u64>,

    #[clap(long, value_name = "NUM")]
    /// Alternative to `end_block` if it's not set
    pub limit: Option<u64>,

    /// Bigtable limit
    #[clap(long, value_name = "NUM", default_value = DEFAULT_BIGTABLE_LIMIT)]
    pub bigtable_limit: usize, // TODO: max_chunk_size
}

#[derive(clap::Args)]
pub struct RestoreChainArgs {
    /// First missing EVM Block
    #[clap(long, value_name = "NUM")]
    pub first_block: u64,

    /// Last missing EVM Block
    #[clap(long, value_name = "NUM")]
    pub last_block: u64,

    /// RPC address of archive node used for restoring EVM Header
    #[clap(long, value_name = "URL", value_hint = clap::ValueHint::Url)]
    pub archive_url: String,

    /// Write restored blocks to Ledger Storage
    #[clap(long)]
    pub modify_ledger: bool,

    /// Continue restoring after tx simulation failures
    #[clap(long)]
    pub force_resume: bool,

    /// TODO: explain JSON schema and reason why this param is required during blocks restore
    #[clap(long, value_name = "FILE_PATH", default_value = "./timestamps/blocks.json", value_hint = clap::ValueHint::FilePath)]
    pub timestamps: String,

    /// Writes restored EVM Blocks as JSON file to directory if set
    #[clap(long, value_name = "DIR", value_hint = clap::ValueHint::DirPath)]
    pub output_dir: Option<String>,

    /// Offset in hours to change timestamp string like "2022-08-16T02:02:04.000Z"
    /// This is useful when timestamp storage use Z as reference to local timestamp instead of UTC.
    #[clap(long, value_name = "OFFSET_HOURS")]
    pub hrs_offset: Option<i64>,
}

#[derive(clap::Args)]
pub struct CheckNativeArgs {
    /// Native Block number
    #[clap(short, long, value_name = "NUM")]
    pub slot: u64,
}

#[derive(clap::Args)]
pub struct CheckEvmArgs {
    /// EVM Block number
    #[clap(short = 'b', long, value_name = "NUM")]
    pub block_number: u64,
}

#[derive(clap::Args)]
pub struct CompareNativeArgs {
    /// First Native Slot
    #[clap(long, value_name = "NUM")]
    pub start_slot: u64,

    /// Limit of Native Blocks to search
    #[clap(long, value_name = "NUM")]
    pub limit: usize,

    /// Google credentials JSON filepath of the "Credible Ledger"
    #[clap(long, value_name = "FILE_PATH", value_hint = clap::ValueHint::FilePath)]
    pub credible_ledger_creds: String,

    /// "Credible Ledger" Instance
    #[clap(long, value_name = "STRING", default_value = DEFAULT_INSTANCE)]
    pub credible_ledger_instance: String,

    /// Google credentials JSON filepath of the "Deceptive Ledger"
    #[clap(long, value_name = "FILE_PATH", value_hint = clap::ValueHint::FilePath)]
    pub dubious_ledger_creds: String,

    /// "Deceptive Ledger" Instance
    #[clap(long, value_name = "STRING", default_value = DEFAULT_INSTANCE)]
    pub dubious_ledger_instance: String,
}

#[derive(clap::Args)]
pub struct UploadArgs {
    /// Path to file with JSON collection of EVM blocks
    #[clap(short, long, value_name = "FILE_PATH", value_hint = clap::ValueHint::FilePath)]
    pub collection: String,
}

#[derive(clap::Args)]
pub struct RepeatEvmArgs {
    /// First EVM Block of the sequence to copy from Src to Dst
    #[clap(short, long, value_name = "NUM")]
    pub block_number: u64,

    /// EVM Block sequence length
    #[clap(short, long, value_name = "NUM", default_value = "1")]
    pub limit: u64,

    /// Google credentials JSON filepath of the Source Ledger
    #[clap(long, value_name = "FILE_PATH", value_hint = clap::ValueHint::FilePath)]
    pub src_creds: String,

    /// Source Ledger Instance
    #[clap(long, value_name = "STRING", default_value = DEFAULT_INSTANCE)]
    pub src_instance: String,

    /// Google credentials JSON filepath of the Destination Ledger
    #[clap(long, value_name = "FILE_PATH", value_hint = clap::ValueHint::FilePath)]
    pub dst_creds: String,

    /// Destination Ledger Instance
    #[clap(long, value_name = "STRING", default_value = DEFAULT_INSTANCE)]
    pub dst_instance: String,
}

#[derive(clap::Args)]
pub struct RepeatNativeArgs {
    /// First Native Block of the sequence to copy from Src to Dst
    #[clap(short, long, value_name = "NUM")]
    pub start_slot: u64,

    /// Native Block sequence length
    #[clap(short, long, value_name = "NUM")]
    pub end_slot: u64,

    /// Google credentials JSON filepath of the Source Ledger
    #[clap(long, value_name = "FILE_PATH", value_hint = clap::ValueHint::FilePath)]
    pub src_creds: String,

    /// Source Ledger Instance
    #[clap(long, value_name = "STRING", default_value = DEFAULT_INSTANCE)]
    pub src_instance: String,

    /// Google credentials JSON filepath of the Destination Ledger
    #[clap(long, value_name = "FILE_PATH", value_hint = clap::ValueHint::FilePath)]
    pub dst_creds: String,

    /// Destination Ledger Instance
    #[clap(long, value_name = "STRING", default_value = DEFAULT_INSTANCE)]
    pub dst_instance: String,
}

#[derive(clap::Args)]
pub struct CompletionArgs {
    /// Whick shell completions to generate
    #[arg(value_enum)]
    #[clap(long, value_name = "STRING")]
    pub shell: clap_complete::Shell,
}
