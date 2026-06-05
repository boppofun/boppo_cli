use anyhow::Context;
use clap::{Args, Parser, Subcommand};
use std::io::Write as _;
use std::path::PathBuf;

use boppo_credential_store::{CredentialStore, DeviceCredentials, default_store_path};
use boppo_device::{browse_mdns, pair_device, sync_dir};
use boppo_device_https_client::{BoppoDevice, BoppoDeviceHttpsClient};
use boppo_usb::{BoppoUsbPort, find_boppo_port};

const MUSIC_DIR: &str = "/sd/activities/user/music";

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
    /// Commands over USB serial
    Usb(UsbArgs),
    /// Discover Boppo devices on the local network via mDNS
    DiscoverDevices,
    /// List the contents of a directory on the device
    LsDir(LsDirArgs),
    /// Remove a file from the device
    RmFile(RmFileArgs),
    /// Download a file from the device to the local machine
    DownloadFile(DownloadFileArgs),
    /// Upload music files to the device
    UploadMusic(UploadMusicArgs),
    /// Print the version and exit
    Version,
}

#[derive(Debug, Args)]
struct UsbArgs {
    /// USB serial port to use (auto-detected if omitted)
    #[arg(long)]
    port: Option<String>,

    #[command(subcommand)]
    command: UsbCommands,
}

#[derive(Debug, Subcommand)]
enum UsbCommands {
    /// Run a shell command on the device over USB serial
    RunCommand {
        /// The command to run
        command: String,
    },
    /// Send Wi-Fi credentials to the device over USB serial
    SendWifi(SendWifiArgs),
}

#[derive(Debug, Args)]
struct SendWifiArgs {
    /// SSID of the network
    ssid: String,
    /// Password (omit for open networks)
    password: Option<String>,
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
struct LsDirArgs {
    /// Path on the device to list
    path: String,
}

#[derive(Debug, Args)]
struct RmFileArgs {
    /// Path of the file to remove on the device
    path: String,
}

#[derive(Debug, Args)]
struct DownloadFileArgs {
    /// Path of the file on the device
    #[arg(long)]
    device_path: String,
    /// Local path to write the file to
    #[arg(long)]
    host_path: String,
}

#[derive(Debug, Args)]
struct UploadMusicArgs {
    /// One or more local music files to upload
    #[arg(required = true)]
    files: Vec<PathBuf>,
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
    /// Base URL of the device (e.g. https://192.168.1.100:443)
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
    /// Base URL of the device (e.g. https://192.168.1.100:443)
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

        Commands::LsDir(args) => {
            let (_, creds) = get_active_device(&store, &cli.device)?;
            let client = BoppoDeviceHttpsClient::new(&creds.url, &creds.password)?;
            let entries = client.read_dir(&args.path).await?;
            if entries.is_empty() {
                println!("(empty)");
            } else {
                for entry in entries {
                    let kind = if entry.is_dir { "d" } else { "f" };
                    println!("{} {:>10}  {}", kind, entry.size, entry.name);
                }
            }
        }

        Commands::RmFile(args) => {
            let (_, creds) = get_active_device(&store, &cli.device)?;
            let client = BoppoDeviceHttpsClient::new(&creds.url, &creds.password)?;
            client.remove_file(&args.path).await?;
            eprintln!("Removed {}", args.path);
        }

        Commands::DownloadFile(args) => {
            let (_, creds) = get_active_device(&store, &cli.device)?;
            let client = BoppoDeviceHttpsClient::new(&creds.url, &creds.password)?;
            let data = client.download_file(&args.device_path).await?;
            std::fs::write(&args.host_path, &data)
                .with_context(|| format!("failed to write {}", args.host_path))?;
            eprintln!("Downloaded {} -> {} ({} bytes)", args.device_path, args.host_path, data.len());
        }

        Commands::UploadMusic(args) => {
            let (_, creds) = get_active_device(&store, &cli.device)?;
            let client = BoppoDeviceHttpsClient::new(&creds.url, &creds.password)?;
            for path in &args.files {
                let raw_name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .with_context(|| format!("invalid file name: {}", path.display()))?;
                let sanitized = sanitize_file_name(raw_name);
                let device_path = format!("{}/{}", MUSIC_DIR, sanitized);
                let data = std::fs::read(path)
                    .with_context(|| format!("failed to read {}", path.display()))?;
                client.upload_file(&device_path, data).await?;
                eprintln!("Uploaded {} -> {}", path.display(), device_path);
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
                eprintln!("Pairing with device {}...", args.serial);
                let password = pair_device(&args.url).await?;
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

        Commands::Version => {
            println!("{}", env!("CARGO_PKG_VERSION"));
        }

        Commands::Usb(usb_args) => {
            let port_path = match usb_args.port {
                Some(p) => p,
                None => find_boppo_port()
                    .context("failed to enumerate USB ports")?
                    .context("no Boppo device found on USB")?,
            };
            eprintln!("Using serial port: {}", port_path);
            let mut port = BoppoUsbPort::open(&port_path)?;
            match usb_args.command {
                UsbCommands::RunCommand { command } => {
                    let output = tokio::task::spawn_blocking(move || port.run_command(&command))
                        .await??;
                    print!("{}", output);
                }
                UsbCommands::SendWifi(args) => {
                    let ssid = args.ssid;
                    let password = args.password;
                    tokio::task::spawn_blocking(move || {
                        port.send_wifi_credentials(&ssid, password.as_deref())
                    })
                    .await??;
                    eprintln!("Wi-Fi credentials sent.");
                }
            }
        }

        Commands::DiscoverDevices => {
            eprintln!("Searching for Boppo devices (5s)...");
            let devices = tokio::task::spawn_blocking(browse_mdns).await??;

            if devices.is_empty() {
                println!("No devices found.");
                return Ok(());
            }

            for device in devices {
                let known = store.get_device(&device.serial).is_some();
                if known {
                    println!(
                        "  {} \"{}\" @ {} [already in store]",
                        device.serial, device.device_name, device.url
                    );
                } else {
                    println!(
                        "  {} \"{}\" @ {} [not paired]",
                        device.serial, device.device_name, device.url
                    );
                    print!("Pair with this device? [Y/n] ");
                    std::io::stdout().flush()?;
                    let mut input = String::new();
                    std::io::stdin().read_line(&mut input)?;
                    let trimmed = input.trim();
                    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("y") {
                        let password = pair_device(&device.url).await?;
                        print!(
                            "Nickname for this device (Enter for \"{}\"): ",
                            device.device_name
                        );
                        std::io::stdout().flush()?;
                        let mut nick_input = String::new();
                        std::io::stdin().read_line(&mut nick_input)?;
                        let nick_input = nick_input.trim();
                        let nickname = if nick_input.is_empty() {
                            Some(device.device_name.clone())
                        } else {
                            Some(nick_input.to_owned())
                        };
                        store.set_device(
                            device.serial.clone(),
                            DeviceCredentials {
                                password,
                                url: device.url.clone(),
                                nickname,
                            },
                        );
                        if store.default.is_none() {
                            store.set_default(device.serial.clone());
                            println!("Set as default device.");
                        }
                        store.save(&store_path)?;
                        println!("Device {} paired and saved.", device.serial);
                    }
                }
            }
        }
    }

    Ok(())
}

/// Mirrors the sanitizeFileName logic in the phone app's files_notifier.dart.
fn sanitize_file_name(name: &str) -> String {
    name.replace('\'', "")
        .replace('\n', "")
        .replace('?', "")
        .replace('/', "")
        .trim()
        .to_owned()
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
