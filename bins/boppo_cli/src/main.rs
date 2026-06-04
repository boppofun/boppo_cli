use anyhow::Context;
use clap::{Args, Parser, Subcommand};
use std::collections::HashMap;
use std::path::PathBuf;

use boppo_credential_store::{CredentialStore, DeviceCredentials, default_store_path};
use boppo_device_https_client::{BoppoDevice, BoppoDeviceHttpsClient, DirEntry};

#[derive(Debug, Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Device serial number or nickname to use
    #[arg(long, global = true)]
    device: Option<String>,

    /// Path to the credential store config file
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Sync a local directory to the device
    SyncDir(SyncDirArgs),
    /// Upload a single file to the device
    UploadFile(UploadFileArgs),
    /// Run a shell command on the device
    RunCommand {
        /// The command to run
        command: String,
    },
    /// Manage registered devices
    Device(DeviceArgs),
}

#[derive(Debug, Args)]
struct SyncDirArgs {
    /// Local directory to sync from
    #[arg(long)]
    host_dir: String,
    /// Device directory to sync to
    #[arg(long)]
    device_dir: String,
    /// Delete extraneous files from destination
    #[arg(short, long, default_value = "false")]
    delete: bool,
    /// Perform a dry run without making changes
    #[arg(short, long, default_value = "false")]
    dry_run: bool,
    /// Print verbose progress messages
    #[arg(short, long, default_value = "false")]
    verbose: bool,
}

#[derive(Debug, Args)]
struct UploadFileArgs {
    /// Local file path to upload
    #[arg(long)]
    host_path: String,
    /// Destination path on the device
    #[arg(long)]
    device_path: String,
}

#[derive(Debug, Args)]
struct DeviceArgs {
    #[command(subcommand)]
    command: DeviceCommands,
}

#[derive(Debug, Subcommand)]
enum DeviceCommands {
    /// List all registered devices
    List,
    /// Add a device to the credential store
    Add(DeviceAddArgs),
    /// Remove a device from the credential store
    Remove {
        /// Device serial number or nickname
        identifier: String,
    },
    /// Set the default device
    SetDefault {
        /// Device serial number or nickname
        identifier: String,
    },
    /// Pair with a new device via the pairing flow
    Pair(DevicePairArgs),
}

#[derive(Debug, Args)]
struct DeviceAddArgs {
    /// Device serial number
    serial: String,
    /// Base URL of the device (e.g. https://192.168.1.100:8080)
    #[arg(long)]
    url: String,
    /// Password / bearer token for the device
    #[arg(long)]
    password: String,
    /// Optional nickname for the device
    #[arg(long)]
    nickname: Option<String>,
}

#[derive(Debug, Args)]
struct DevicePairArgs {
    /// Device serial number
    serial: String,
    /// Base URL of the device (e.g. https://192.168.1.100:8080)
    #[arg(long)]
    url: String,
    /// Optional nickname for the device
    #[arg(long)]
    nickname: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let store_path = cli.config.unwrap_or_else(default_store_path);
    let mut store = CredentialStore::load(&store_path)?;

    match cli.command {
        Commands::SyncDir(args) => {
            let (_, creds) = get_active_device(&store, &cli.device)?;
            let client = BoppoDeviceHttpsClient::new(&creds.url, &creds.password)?;
            if args.dry_run {
                eprintln!("Dry run...");
            }
            sync_dir(
                &client,
                &args.host_dir,
                &args.device_dir,
                args.delete,
                args.dry_run,
                args.verbose,
            )
            .await?;
            if args.verbose {
                eprintln!("Done syncing all files.");
            }
        }

        Commands::UploadFile(args) => {
            let (_, creds) = get_active_device(&store, &cli.device)?;
            let client = BoppoDeviceHttpsClient::new(&creds.url, &creds.password)?;
            let data = std::fs::read(&args.host_path)
                .with_context(|| format!("failed to read {}", args.host_path))?;
            client.upload_file(&args.device_path, data).await?;
            eprintln!("Uploaded {} -> {}", args.host_path, args.device_path);
        }

        Commands::RunCommand { command } => {
            let (_, creds) = get_active_device(&store, &cli.device)?;
            let client = BoppoDeviceHttpsClient::new(&creds.url, &creds.password)?;
            let output = client.run_command(&command).await?;
            print!("{}", output);
        }

        Commands::Device(device_args) => match device_args.command {
            DeviceCommands::List => {
                if store.devices.is_empty() {
                    println!("No devices registered.");
                } else {
                    for (serial, creds) in &store.devices {
                        let is_default = store.default.as_deref() == Some(serial.as_str());
                        let default_marker = if is_default { " [default]" } else { "" };
                        let nickname = creds.nickname.as_deref().unwrap_or("(none)");
                        println!(
                            "{}{} | url: {} | nickname: {}",
                            serial, default_marker, creds.url, nickname
                        );
                    }
                }
            }

            DeviceCommands::Add(args) => {
                let creds = DeviceCredentials {
                    password: args.password,
                    url: args.url,
                    nickname: args.nickname,
                };
                store.set_device(&args.serial, creds);
                store.save(&store_path)?;
                println!("Device {} added.", args.serial);
            }

            DeviceCommands::Remove { identifier } => {
                let serial = store
                    .resolve_device(&identifier)
                    .map(|(s, _)| s.to_owned())
                    .with_context(|| format!("device '{}' not found", identifier))?;
                let removed = store.remove_device(&serial);
                if removed {
                    // If we removed the default, clear it
                    if store.default.as_deref() == Some(serial.as_str()) {
                        store.clear_default();
                    }
                    store.save(&store_path)?;
                    println!("Device {} removed.", serial);
                } else {
                    anyhow::bail!("device '{}' not found", identifier);
                }
            }

            DeviceCommands::SetDefault { identifier } => {
                let serial = store
                    .resolve_device(&identifier)
                    .map(|(s, _)| s.to_owned())
                    .with_context(|| format!("device '{}' not found", identifier))?;
                store.set_default(&serial);
                store.save(&store_path)?;
                println!("Default device set to {}.", serial);
            }

            DeviceCommands::Pair(args) => {
                let request_id = format!(
                    "boppo-cli-{}",
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs()
                );
                eprintln!(
                    "Pairing with device {}...",
                    args.serial
                );
                eprintln!(
                    "Please approve the pairing request on your device. Request ID: {}",
                    request_id
                );
                let password =
                    boppo_device_https_client::get_password(&args.url, &request_id).await?;
                let creds = DeviceCredentials {
                    password,
                    url: args.url,
                    nickname: args.nickname,
                };
                store.set_device(&args.serial, creds);
                store.save(&store_path)?;
                println!("Device {} paired and saved.", args.serial);
            }
        },
    }

    Ok(())
}

fn get_active_device<'a>(
    store: &'a CredentialStore,
    device_arg: &Option<String>,
) -> anyhow::Result<(&'a str, &'a DeviceCredentials)> {
    if let Some(identifier) = device_arg {
        store
            .resolve_device(identifier)
            .with_context(|| format!("device '{}' not found in credential store", identifier))
    } else {
        store
            .default_device()
            .context("no default device set; use --device or `boppo device set-default`")
    }
}

#[async_recursion::async_recursion]
async fn sync_dir(
    device: &dyn BoppoDevice,
    host_dir: &str,
    device_dir: &str,
    delete: bool,
    dry_run: bool,
    verbose: bool,
) -> anyhow::Result<()> {
    if verbose {
        eprintln!("Syncing {}", device_dir);
    }

    let host_entries = list_host_dir(host_dir)?;
    let device_entries_vec = device.read_dir(device_dir).await?;
    let device_entries: HashMap<String, &DirEntry> =
        device_entries_vec.iter().map(|e| (e.name.clone(), e)).collect();

    // Upload files that are missing or have different sizes
    for (name, host_attr) in &host_entries {
        if host_attr.is_dir {
            continue;
        }
        let reason = match device_entries.get(name) {
            Some(device_entry) => {
                if device_entry.size == host_attr.size {
                    continue;
                }
                "sizes are different"
            }
            None => "file is missing on device",
        };
        let host_path = format!("{}/{}", host_dir, name);
        let device_path = format!("{}/{}", device_dir, name);
        let contents = std::fs::read(&host_path)
            .with_context(|| format!("failed to read {}", host_path))?;
        eprintln!(
            "Uploading {} bytes because {}\n\tfrom: {}\n\tto: {}",
            contents.len(),
            reason,
            host_path,
            device_path,
        );
        if !dry_run {
            device
                .upload_file(&device_path, contents)
                .await
                .with_context(|| format!("failed to upload {}", host_path))?;
        }
    }

    // Recurse into subdirectories
    for (name, host_attr) in &host_entries {
        if !host_attr.is_dir {
            continue;
        }
        let new_host_dir = format!("{}/{}", host_dir, name);
        let new_device_dir = format!("{}/{}", device_dir, name);
        sync_dir(device, &new_host_dir, &new_device_dir, delete, dry_run, verbose)
            .await
            .with_context(|| format!("failed to sync host dir: {}", host_dir))?;
    }

    // Delete extraneous files from device
    if delete {
        for (name, device_entry) in &device_entries {
            if device_entry.is_dir {
                continue;
            }
            if host_entries.contains_key(name) {
                continue;
            }
            let device_path = format!("{}/{}", device_dir, name);
            eprintln!("Removing {}", device_path);
            if !dry_run {
                device
                    .remove_file(&device_path)
                    .await
                    .with_context(|| format!("failed to remove {}", device_path))?;
            }
        }
    }

    Ok(())
}

struct HostFileAttr {
    size: u64,
    is_dir: bool,
}

fn list_host_dir(host_dir: &str) -> anyhow::Result<HashMap<String, HostFileAttr>> {
    let mut entries = HashMap::new();
    for entry in std::fs::read_dir(host_dir)
        .with_context(|| format!("failed to read host directory {}", host_dir))?
    {
        let entry = entry?;
        let metadata = entry.metadata()?;
        let is_dir = metadata.is_dir();
        let size = metadata.len();
        let file_name = entry
            .path()
            .file_name()
            .unwrap()
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("non UTF-8 file name"))?
            .to_owned();
        entries.insert(file_name, HostFileAttr { size, is_dir });
    }
    Ok(entries)
}
