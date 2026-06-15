mod wasm;

use anyhow::Context;
use clap::{Args, CommandFactory, FromArgMatches, Parser, Subcommand};
use clap_complete::{Shell, generate};
use std::ffi::OsStr;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::sync::Arc;

use boppo_cli::credential_store::{CredentialStore, DeviceCredentials, default_store_path, device_url};
use boppo_cli::device::{SyncStatus, browse_mdns, pair_device, sync_dir};
use boppo_cli::device_https_client::{
    BoppoDevice, BoppoDeviceHttpsClient, DeviceError, ProgressCallback, ProgressFactory,
};
use boppo_cli::usb::{BoppoUsbPort, find_boppo_port};

const MUSIC_DIR: &str = "/sd/activities/user/music";

#[derive(Debug, Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Device serial number or nickname to use
    #[arg(long, global = true)]
    device: Option<String>,

    /// Config file path
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Commands over Wi-Fi (HTTPS)
    Wifi(WifiArgs),
    /// Commands over USB serial
    Usb(UsbArgs),
    /// Manage registered devices
    Devices(DevicesArgs),
    /// Activity development commands (build, deploy, start)
    Activity(ActivityArgs),
    /// Print the version and exit
    Version,
    /// Print shell completion script to stdout
    Completions {
        /// Shell to generate completions for
        shell: Shell,
    },
}

// ── Wi-Fi ────────────────────────────────────────────────────────────────────

#[derive(Debug, Args)]
struct WifiArgs {
    #[command(subcommand)]
    command: WifiCommands,
}

#[derive(Debug, Subcommand)]
enum WifiCommands {
    /// Sync a local directory to the device
    SyncDir(SyncDirArgs),
    /// Upload a single file to the device
    UploadFile(UploadFileArgs),
    /// Upload music files to the device
    UploadMusic(UploadMusicArgs),
    /// Download a file from the device to the local machine
    DownloadFile(DownloadFileArgs),
    /// List the contents of a directory on the device
    LsDir(LsDirArgs),
    /// Remove a file from the device
    RmFile(RmFileArgs),
    /// Run a shell command on the device
    ExecuteCommand {
        /// The command to run
        command: String,
    },
    /// Discover Boppo devices on the local network via mDNS
    Discover,
    /// Pair with a new device via the pairing flow
    Pair(DevicePairArgs),
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
    #[arg(short = 'n', long, default_value = "false")]
    dry_run: bool,
    /// Print a summary line when finished
    #[arg(short, long, default_value = "false")]
    verbose: bool,
    /// Disable per-file progress bars
    #[arg(long, default_value = "false")]
    no_progress: bool,
}

#[derive(Debug, Args)]
struct UploadFileArgs {
    /// Local file path to upload
    #[arg(long)]
    host_path: String,
    /// Destination path on the device
    #[arg(long)]
    device_path: String,
    /// Disable the progress bar
    #[arg(long, default_value = "false")]
    no_progress: bool,
}

#[derive(Debug, Args)]
struct UploadMusicArgs {
    /// One or more local music files to upload
    #[arg(required = true)]
    files: Vec<PathBuf>,
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
struct LsDirArgs {
    /// Path on the device to list
    path: String,
}

#[derive(Debug, Args)]
struct RmFileArgs {
    /// Path of the file to remove on the device
    path: String,
}

// ── USB ──────────────────────────────────────────────────────────────────────

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
    ExecuteCommand {
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

// ── Activity ─────────────────────────────────────────────────────────────────

#[derive(Debug, Args)]
struct ActivityArgs {
    #[command(subcommand)]
    command: ActivityCommands,
}

#[derive(Debug, Subcommand)]
enum ActivityCommands {
    /// Compile the WebAssembly activity in the current directory
    Build(BuildArgs),
    /// Build, deploy, and start the activity (all three by default)
    Deploy(DeployArgs),
    /// Start the deployed activity on the device
    Start,
}

#[derive(Debug, Args)]
struct BuildArgs {
    /// Skip wasm-opt optimization
    #[arg(long)]
    no_optimize: bool,
}

#[derive(Debug, Args)]
struct DeployArgs {
    /// Skip wasm-opt optimization
    #[arg(long)]
    no_optimize: bool,
    /// Delete files on the device that are no longer in the local build
    #[arg(long)]
    delete: bool,
    /// Skip the build step (deploy whatever is already compiled)
    #[arg(long)]
    no_build: bool,
    /// Skip starting the activity after deploying
    #[arg(long)]
    no_start: bool,
}

// ── Devices ──────────────────────────────────────────────────────────────────

#[derive(Debug, Args)]
struct DevicesArgs {
    #[command(subcommand)]
    command: DevicesCommands,
}

#[derive(Debug, Subcommand)]
enum DevicesCommands {
    /// List all registered devices
    List(DevicesListArgs),
    /// Print the serial number and password for the active device
    Get(DevicesGetArgs),
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
}

#[derive(Debug, Args)]
struct DevicesListArgs {
    /// Output as JSON
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct DevicesGetArgs {
    /// Output as JSON
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct DeviceAddArgs {
    /// Device serial number
    serial: String,
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
}

// ── Main ─────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let default_path: &'static OsStr =
        Box::leak(default_store_path().into_os_string().into_boxed_os_str());
    let matches = Cli::command()
        .mut_arg("config", |a| a.default_value_os(default_path))
        .get_matches();
    let cli = Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());

    let store_path = cli.config.unwrap_or_else(default_store_path);
    let mut store = CredentialStore::load(&store_path)?;

    match cli.command {
        Commands::Wifi(wifi_args) => {
            let active_serial = get_active_device(&store, &cli.device)
                .ok()
                .map(|(s, _)| s.to_owned());
            let result = run_wifi_commands(&mut store, &cli.device, &store_path, wifi_args).await;
            if let Err(ref e) = result
                && try_repair_on_unauthorized(e, &mut store, &active_serial, &store_path).await?
            {
                return Ok(());
            }
            result?;
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
                UsbCommands::ExecuteCommand { command } => {
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

        Commands::Devices(device_args) => match device_args.command {
            DevicesCommands::List(args) => {
                if args.json {
                    let entries: Vec<_> = store.devices.iter().map(|(serial, creds)| {
                        serde_json::json!({
                            "serial": serial,
                            "nickname": creds.nickname,
                            "default": store.default.as_deref() == Some(serial.as_str()),
                        })
                    }).collect();
                    println!("{}", serde_json::to_string_pretty(&entries)?);
                } else if store.devices.is_empty() {
                    println!("No devices registered.");
                } else {
                    for (serial, creds) in &store.devices {
                        let is_default = store.default.as_deref() == Some(serial.as_str());
                        let default_marker = if is_default { " [default]" } else { "" };
                        let nickname = creds.nickname.as_deref().unwrap_or("(none)");
                        println!("{} | nickname: {}{}", serial, nickname, default_marker);
                    }
                }
            }

            DevicesCommands::Get(args) => {
                let (serial, creds) = get_active_device(&store, &cli.device)?;
                if args.json {
                    println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                        "serial": serial,
                        "password": creds.password,
                        "nickname": creds.nickname,
                        "default": store.default.as_deref() == Some(serial),
                    }))?);
                } else {
                    println!("serial:   {}", serial);
                    println!("password: {}", creds.password);
                }
            }

            DevicesCommands::Add(args) => {
                let creds = DeviceCredentials {
                    password: args.password,
                    nickname: args.nickname,
                };
                store.set_device(&args.serial, creds);
                store.save(&store_path)?;
                println!("Device {} added.", args.serial);
            }

            DevicesCommands::Remove { identifier } => {
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

            DevicesCommands::SetDefault { identifier } => {
                let serial = store
                    .resolve_device(&identifier)
                    .map(|(s, _)| s.to_owned())
                    .with_context(|| format!("device '{}' not found", identifier))?;
                store.set_default(&serial);
                store.save(&store_path)?;
                println!("Default device set to {}.", serial);
            }

        },

        Commands::Completions { shell } => {
            let mut cmd = Cli::command();
            generate(shell, &mut cmd, "boppo", &mut std::io::stdout());
            let install_hint = match shell {
                Shell::Fish =>    "boppo completions fish > ~/.config/fish/completions/boppo.fish",
                Shell::Bash =>    "boppo completions bash > ~/.local/share/bash-completion/completions/boppo",
                Shell::Zsh =>     "boppo completions zsh > ~/.zfunc/_boppo",
                Shell::Elvish =>  "boppo completions elvish >> ~/.config/elvish/lib/completions.elv",
                Shell::PowerShell => "boppo completions powershell >> $PROFILE",
                _ =>              "pipe this output to your shell's completion directory",
            };
            println!("\n# To install: {install_hint}");
        }

        Commands::Activity(activity_args) => {
            let active_serial = get_active_device(&store, &cli.device)
                .ok()
                .map(|(s, _)| s.to_owned());
            let result = run_activity_commands(&mut store, &cli.device, activity_args).await;
            if let Err(ref e) = result
                && try_repair_on_unauthorized(e, &mut store, &active_serial, &store_path).await?
            {
                return Ok(());
            }
            result?;
        }

        Commands::Version => {
            println!("{}", env!("CARGO_PKG_VERSION"));
        }
    }

    Ok(())
}

async fn run_wifi_commands(
    store: &mut CredentialStore,
    device_arg: &Option<String>,
    store_path: &Path,
    wifi_args: WifiArgs,
) -> anyhow::Result<()> {
    match wifi_args.command {
        WifiCommands::SyncDir(args) => {
            let (serial, creds) = get_active_device(store, device_arg)?;
            let client = BoppoDeviceHttpsClient::new(device_url(serial), &creds.password)?;
            if args.dry_run {
                eprintln!("Dry run...");
            }
            if args.no_progress {
                sync_dir(&client, &args.host_dir, &args.device_dir, args.delete, args.dry_run, None, None).await?;
            } else {
                sync_with_progress(&client, &args.host_dir, &args.device_dir, args.delete, args.dry_run).await?;
            }
            if args.verbose {
                eprintln!("Done syncing all files.");
            }
        }

        WifiCommands::UploadFile(args) => {
            let (serial, creds) = get_active_device(store, device_arg)?;
            let client = BoppoDeviceHttpsClient::new(device_url(serial), &creds.password)?;
            let data = std::fs::read(&args.host_path)
                .with_context(|| format!("failed to read {}", args.host_path))?;
            if args.no_progress {
                client.upload_file(&args.device_path, data, None).await?;
                eprintln!("Uploaded {} -> {}", args.host_path, args.device_path);
            } else {
                let label = std::path::Path::new(&args.host_path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(&args.host_path)
                    .to_owned();
                let pb = ProgressBar::new(data.len() as u64);
                pb.set_style(upload_style());
                pb.set_message(label);
                let pb2 = pb.clone();
                let progress: ProgressCallback =
                    Arc::new(move |sent: u64, _total: u64| pb2.set_position(sent));
                client.upload_file(&args.device_path, data, Some(progress)).await?;
                pb.finish_with_message("done");
                eprintln!("Uploaded {} -> {}", args.host_path, args.device_path);
            }
        }

        WifiCommands::UploadMusic(args) => {
            let (serial, creds) = get_active_device(store, device_arg)?;
            let client = BoppoDeviceHttpsClient::new(device_url(serial), &creds.password)?;
            for path in &args.files {
                let raw_name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .with_context(|| format!("invalid file name: {}", path.display()))?;
                let sanitized = sanitize_file_name(raw_name);
                let device_path = format!("{}/{}", MUSIC_DIR, sanitized);
                let data = std::fs::read(path)
                    .with_context(|| format!("failed to read {}", path.display()))?;
                client.upload_file(&device_path, data, None).await?;
                eprintln!("Uploaded {} -> {}", path.display(), device_path);
            }
        }

        WifiCommands::DownloadFile(args) => {
            let (serial, creds) = get_active_device(store, device_arg)?;
            let client = BoppoDeviceHttpsClient::new(device_url(serial), &creds.password)?;
            let data = client.download_file(&args.device_path).await?;
            std::fs::write(&args.host_path, &data)
                .with_context(|| format!("failed to write {}", args.host_path))?;
            eprintln!(
                "Downloaded {} -> {} ({} bytes)",
                args.device_path,
                args.host_path,
                data.len()
            );
        }

        WifiCommands::LsDir(args) => {
            let (serial, creds) = get_active_device(store, device_arg)?;
            let client = BoppoDeviceHttpsClient::new(device_url(serial), &creds.password)?;
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

        WifiCommands::RmFile(args) => {
            let (serial, creds) = get_active_device(store, device_arg)?;
            let client = BoppoDeviceHttpsClient::new(device_url(serial), &creds.password)?;
            client.remove_file(&args.path).await?;
            eprintln!("Removed {}", args.path);
        }

        WifiCommands::ExecuteCommand { command } => {
            let (serial, creds) = get_active_device(store, device_arg)?;
            let client = BoppoDeviceHttpsClient::new(device_url(serial), &creds.password)?;
            let output = client.run_command(&command).await?;
            print!("{}", output);
        }

        WifiCommands::Pair(args) => {
            let existing_nickname = store.get_device(&args.serial).and_then(|c| c.nickname.clone());
            let nickname = prompt_nickname(existing_nickname.as_deref())?;
            pair_and_save(store, &args.serial, store_path, nickname).await?;
        }

        WifiCommands::Discover => {
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
                        "  {} \"{}\" [already in store]",
                        device.serial, device.device_name
                    );
                } else {
                    println!(
                        "  {} \"{}\" [not paired]",
                        device.serial, device.device_name
                    );
                    print!("Pair with this device? [Y/n] ");
                    std::io::stdout().flush()?;
                    let mut input = String::new();
                    std::io::stdin().read_line(&mut input)?;
                    let trimmed = input.trim();
                    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("y") {
                        let nickname = prompt_nickname(Some(&device.device_name))?;
                        if store.default.is_none() {
                            store.set_default(&device.serial);
                            println!("Set as default device.");
                        }
                        pair_and_save(store, &device.serial, store_path, nickname).await?;
                    }
                }
            }
        }

    }
    Ok(())
}

async fn run_activity_commands(
    store: &mut CredentialStore,
    device_arg: &Option<String>,
    activity_args: ActivityArgs,
) -> anyhow::Result<()> {
    match activity_args.command {
        ActivityCommands::Build(args) => {
            wasm::build(!args.no_optimize)?;
        }

        ActivityCommands::Deploy(args) => {
            let package_name = if args.no_build {
                wasm::package_name()?
            } else {
                wasm::build(!args.no_optimize)?
            };

            let (serial, creds) = get_active_device(store, device_arg)?;
            let client = BoppoDeviceHttpsClient::new(device_url(serial), &creds.password)?;

            let staging = wasm::stage(&package_name)?;
            let device_dir = format!("{}/{}", wasm::DEVICE_ROOT, package_name);
            sync_with_progress(&client, staging.to_str().unwrap(), &device_dir, args.delete, false).await?;
            eprintln!("Deployed {} to {}", package_name, device_dir);

            if !args.no_start {
                client.run_command(&wasm::start_command(&package_name)).await?;
                eprintln!("Started {}", package_name);
            }
        }

        ActivityCommands::Start => {
            let package_name = wasm::package_name()?;
            let (serial, creds) = get_active_device(store, device_arg)?;
            let client = BoppoDeviceHttpsClient::new(device_url(serial), &creds.password)?;
            client.run_command(&wasm::start_command(&package_name)).await?;
            eprintln!("Started {}", package_name);
        }
    }
    Ok(())
}

/// If `err` is a 401, prompts the user to re-pair. Returns `true` if re-pairing succeeded
/// (caller should return `Ok(())`), `false` if the error wasn't a 401 or the user declined.
async fn try_repair_on_unauthorized(
    err: &anyhow::Error,
    store: &mut CredentialStore,
    active_serial: &Option<String>,
    store_path: &Path,
) -> anyhow::Result<bool> {
    if !is_unauthorized(err) {
        return Ok(false);
    }
    eprintln!("Error: unauthorized (401) — is the password correct?");
    if let Some(serial) = active_serial {
        print!("Re-pair with device {}? [Y/n] ", serial);
        std::io::stdout().flush()?;
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        let trimmed = input.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("y") {
            let nickname = store.get_device(serial).and_then(|c| c.nickname.clone());
            pair_and_save(store, serial, store_path, nickname).await?;
            return Ok(true);
        }
    }
    Ok(false)
}

async fn sync_with_progress(
    client: &BoppoDeviceHttpsClient,
    host_dir: &str,
    device_dir: &str,
    delete: bool,
    dry_run: bool,
) -> anyhow::Result<()> {
    let mp = MultiProgress::new();
    let dir_bar = mp.add(ProgressBar::new_spinner());
    dir_bar.set_style(ProgressStyle::with_template("  {spinner:.dim} Syncing {msg}").unwrap());
    dir_bar.enable_steady_tick(std::time::Duration::from_millis(200));

    let dir_bar_for_status = dir_bar.clone();
    let dir_bar_for_println = dir_bar.clone();
    let status = SyncStatus {
        set_dir: Arc::new(move |dir: &str| dir_bar_for_status.set_message(dir.to_owned())),
        println: Arc::new(move |line: &str| dir_bar_for_println.println(line)),
    };

    let dir_bar_for_factory = dir_bar.clone();
    let mp_for_factory = mp.clone();
    let factory: ProgressFactory = Arc::new(move |device_path: &str, total: u64| -> ProgressCallback {
        let label = std::path::Path::new(device_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(device_path)
            .to_owned();
        let device_path = device_path.to_owned();
        let pb = mp_for_factory.insert_after(&dir_bar_for_factory, ProgressBar::new(total));
        pb.set_style(upload_style());
        pb.set_message(label);
        let pb2 = pb.clone();
        let dir_bar_in_cb = dir_bar_for_factory.clone();
        Arc::new(move |sent: u64, total: u64| {
            pb2.set_position(sent);
            if sent >= total {
                pb2.finish_and_clear();
                dir_bar_in_cb.println(format!("Uploaded {}", device_path));
            }
        })
    });

    sync_dir(client, host_dir, device_dir, delete, dry_run, Some(factory), Some(status)).await?;
    dir_bar.finish_and_clear();
    Ok(())
}

async fn pair_and_save(
    store: &mut CredentialStore,
    serial: &str,
    store_path: &Path,
    nickname: Option<String>,
) -> anyhow::Result<()> {
    eprintln!("Pairing with device {}...", serial);
    let password = pair_device(serial).await?;
    store.set_device(serial, DeviceCredentials { password, nickname });
    store.save(store_path)?;
    println!("Device {} paired and saved.", serial);
    Ok(())
}

fn prompt_nickname(default: Option<&str>) -> anyhow::Result<Option<String>> {
    let prompt = match default {
        Some(nick) => format!("Nickname (Enter for \"{}\"): ", nick),
        None => "Nickname (Enter to skip): ".to_owned(),
    };
    print!("{}", prompt);
    std::io::stdout().flush()?;
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let input = input.trim();
    Ok(if input.is_empty() {
        default.map(str::to_owned)
    } else {
        Some(input.to_owned())
    })
}

fn upload_style() -> ProgressStyle {
    ProgressStyle::with_template(
        "{msg:.bold}  [{wide_bar:.cyan/blue}]  {bytes}/{total_bytes}  {binary_bytes_per_sec}",
    )
    .unwrap()
    .progress_chars("=>-")
}

/// Returns true if the error chain contains a 401 Unauthorized device error.
fn is_unauthorized(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<DeviceError>()
            .is_some_and(|e| matches!(e, DeviceError::Unauthorized))
    })
}

/// Mirrors the sanitizeFileName logic in the phone app's files_notifier.dart.
fn sanitize_file_name(name: &str) -> String {
    name.replace(['\'', '\n', '?', '/'], "")
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
            .context("no default device set; use --device or `boppo devices set-default`")
    }
}
