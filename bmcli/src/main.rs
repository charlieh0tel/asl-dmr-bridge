//! Brandmeister API CLI: query / manage one peer's static talkgroups
//! plus a few diagnostic reads.
//!
//! Token resolution (mutually exclusive, picked in this order):
//!   1. --api-key-file <path>
//!   2. BRANDMEISTER_API_KEY env var
//!
//! Read-only commands (device, profile, statics list, talkgroup info)
//! work without a token.  Anything that mutates state, or the
//! `getRepeater` / `drop-dynamic` actions, requires one.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use clap::Args;
use clap::Parser;
use clap::Subcommand;
use secrecy::SecretString;

use brandmeister_api::client::Client;
use brandmeister_api::types::DeviceId;
use brandmeister_api::types::Slot;
use brandmeister_api::types::TalkgroupId;

const ENV_API_KEY: &str = "BRANDMEISTER_API_KEY";

#[derive(Parser)]
#[command(name = "bmcli", about = "Brandmeister API CLI", version)]
struct Cli {
    /// Read the API key from this file (single-line bearer JWT).
    /// Mutually exclusive with $BRANDMEISTER_API_KEY.
    #[arg(long, global = true)]
    api_key_file: Option<PathBuf>,

    #[command(subcommand)]
    cmd: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Device (peer / hotspot / repeater) operations.
    Device(DeviceArgs),
    /// Talkgroup operations (info + subscriber list).
    Talkgroup(TalkgroupArgs),
}

#[derive(Args)]
struct DeviceArgs {
    /// Device (peer) ID.  E.g. 310770201 for an AI6KG hotspot.
    id: DeviceId,
    #[command(subcommand)]
    cmd: DeviceCmd,
}

#[derive(Subcommand)]
enum DeviceCmd {
    /// Show device info (callsign, last master, freqs, status).
    Info,
    /// Show full profile: statics, dynamics, timed, blocks, cluster.
    Profile,
    /// List static talkgroups currently subscribed.
    Statics,
    /// Manage static talkgroup subscriptions.
    Static {
        #[command(subcommand)]
        cmd: StaticCmd,
    },
    /// Live state from the master the peer is connected to.
    /// Requires API key.
    GetRepeater,
    /// Drop all dynamic subscriptions on a slot.  Requires API key.
    DropDynamic {
        #[arg(long)]
        slot: Slot,
    },
}

#[derive(Subcommand)]
enum StaticCmd {
    /// Add a static talkgroup subscription (slot + group).  Requires API key.
    Add {
        #[arg(long)]
        slot: Slot,
        #[arg(long)]
        tg: TalkgroupId,
    },
    /// Remove a static talkgroup subscription.  Requires API key.
    Remove {
        #[arg(long)]
        slot: Slot,
        #[arg(long)]
        tg: TalkgroupId,
    },
}

#[derive(Args)]
struct TalkgroupArgs {
    /// Talkgroup ID.  E.g. 91 for worldwide chat, 9990 for parrot.
    id: TalkgroupId,
    #[command(subcommand)]
    cmd: TalkgroupCmd,
}

#[derive(Subcommand)]
enum TalkgroupCmd {
    /// Show talkgroup metadata (name, description, language).
    Info,
    /// List devices that have this talkgroup statically subscribed.
    Devices,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<()> {
    let token = resolve_token(cli.api_key_file.as_deref()).await?;
    let client = match token {
        Some(t) => Client::with_token(t),
        None => Client::new(),
    };
    match cli.cmd {
        Command::Device(d) => run_device(&client, d).await,
        Command::Talkgroup(t) => run_talkgroup(&client, t).await,
    }
}

async fn run_device(client: &Client, args: DeviceArgs) -> Result<()> {
    match args.cmd {
        DeviceCmd::Info => {
            let dev = client.device(args.id).await?;
            print_json(&dev)
        }
        DeviceCmd::Profile => {
            let prof = client.device_profile(args.id).await?;
            print_json(&prof)
        }
        DeviceCmd::Statics => {
            let statics = client.device_talkgroups(args.id).await?;
            print_json(&statics)
        }
        DeviceCmd::Static { cmd } => match cmd {
            StaticCmd::Add { slot, tg } => {
                client.add_static_talkgroup(args.id, slot, tg).await?;
                println!("added static TG {tg} on slot {slot} for device {}", args.id);
                Ok(())
            }
            StaticCmd::Remove { slot, tg } => {
                client.remove_static_talkgroup(args.id, slot, tg).await?;
                println!(
                    "removed static TG {tg} on slot {slot} from device {}",
                    args.id
                );
                Ok(())
            }
        },
        DeviceCmd::GetRepeater => {
            let v = client.get_repeater(args.id).await?;
            print_json(&v)
        }
        DeviceCmd::DropDynamic { slot } => {
            client.drop_dynamic_groups(args.id, slot).await?;
            println!(
                "dropped dynamic groups on slot {slot} for device {}",
                args.id
            );
            Ok(())
        }
    }
}

async fn run_talkgroup(client: &Client, args: TalkgroupArgs) -> Result<()> {
    match args.cmd {
        TalkgroupCmd::Info => {
            let tg = client.talkgroup(args.id).await?;
            print_json(&tg)
        }
        TalkgroupCmd::Devices => {
            let devs = client.talkgroup_devices(args.id).await?;
            print_json(&devs)
        }
    }
}

fn print_json<T: serde::Serialize>(v: &T) -> Result<()> {
    let s = serde_json::to_string_pretty(v).context("encode JSON for stdout")?;
    println!("{s}");
    Ok(())
}

/// Resolve the API key from --api-key-file or $BRANDMEISTER_API_KEY.
/// At most one source may be set; setting both is a startup error
/// (matches the bridge's password-source policy).
async fn resolve_token(file: Option<&std::path::Path>) -> Result<Option<SecretString>> {
    let env_value = std::env::var(ENV_API_KEY).ok().filter(|s| !s.is_empty());
    match (file, env_value) {
        (Some(_), Some(_)) => Err(anyhow!("set --api-key-file or {ENV_API_KEY}, not both")),
        (Some(path), None) => {
            let raw = tokio::fs::read_to_string(path)
                .await
                .with_context(|| format!("read API key file {}", path.display()))?;
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Err(anyhow!("API key file {} is empty", path.display()));
            }
            if trimmed.contains('\n') {
                return Err(anyhow!("API key file {} contains newlines", path.display()));
            }
            Ok(Some(SecretString::from(trimmed.to_owned())))
        }
        (None, Some(value)) => Ok(Some(SecretString::from(value))),
        (None, None) => Ok(None),
    }
}
