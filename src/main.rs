use anyhow::Context;
use clap::Parser;
use reqwest::{Client, ClientBuilder};
use std::collections::HashMap;

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    let args = CliArgs::parse();
    if args.dry_run {
        eprintln!("Dry run...");
    }
    let client = new_client();
    sync_dir(
        &args.device_url,
        &args.host_dir,
        &args.device_dir,
        args.delete,
        args.dry_run,
        args.verbose,
        &args.bearer_token,
        &client,
    )
    .await?;
    if args.verbose {
        eprintln!("Done syncing all files.");
    }
    Ok(())
}

#[derive(Debug, Parser)]
#[command(author, version, about, long_about = None)]
struct CliArgs {
    #[arg(long)]
    device_url: String,
    #[arg(long)]
    host_dir: String,
    #[arg(long)]
    device_dir: String,
    #[arg(long)]
    bearer_token: String,
    #[arg(
        short,
        long,
        default_value = "false",
        help = "delete extraneous files from dest dirs"
    )]
    delete: bool,
    #[arg(short, long, default_value = "false")]
    dry_run: bool,
    #[arg(short, long, default_value = "false")]
    verbose: bool,
}

#[derive(Debug)]
struct FileAttributes {
    size: u64,
    is_dir: bool,
}

#[async_recursion::async_recursion]
async fn sync_dir(
    device_url: &str,
    host_dir: &str,
    device_dir: &str,
    delete: bool,
    dry_run: bool,
    verbose: bool,
    bearer_token: &str,
    client: &Client,
) -> anyhow::Result<()> {
    let host_files = list_host_dir(host_dir).await?;
    let device_files = list_remote_dir(device_url, device_dir, bearer_token, client).await?;
    if verbose {
        eprintln!("Syncing {} to {}", host_dir, device_dir);
    }
    for host_file in &host_files {
        if host_file.1.is_dir {
            continue;
        }
        let name = &host_file.0;
        let reason = match device_files.get(name.as_str()) {
            Some(device_file) => {
                if device_file.size == host_file.1.size {
                    continue;
                }
                "sizes are different"
            }
            None => "file is missing on device",
        };
        transfer_file(
            device_url,
            name,
            host_dir,
            device_dir,
            reason,
            dry_run,
            bearer_token,
            client,
        )
        .await
        .with_context(|| format!("failed to upload {}/{}", host_dir, name))?;
    }
    for host_file in &host_files {
        if !host_file.1.is_dir {
            continue;
        }
        let name = &host_file.0;
        let new_host_dir = format!("{host_dir}/{name}");
        let new_device_dir = format!("{device_dir}/{name}");
        sync_dir(
            device_url,
            &new_host_dir,
            &new_device_dir,
            delete,
            dry_run,
            verbose,
            bearer_token,
            client,
        )
        .await
        .with_context(|| format!("failed to sync host dir: {}", host_dir))?;
    }
    if delete {
        for device_file in &device_files {
            if device_file.1.is_dir {
                continue;
            }
            let name = &device_file.0;
            if host_files.contains_key(name.as_str()) {
                continue;
            }
            let device_path = format!("{device_dir}/{name}");
            eprintln!("Removing {}", device_path);
            if !dry_run {
                remove_file(device_url, &device_path, bearer_token, client).await?;
            }
        }
    }
    Ok(())
}

async fn transfer_file(
    device_url: &str,
    name: &str,
    host_dir: &str,
    device_dir: &str,
    reason: &str,
    dry_run: bool,
    bearer_token: &str,
    client: &Client,
) -> anyhow::Result<()> {
    let host_path = format!("{host_dir}/{name}");
    let contents = std::fs::read(&host_path)?;
    let device_path = format!("{device_dir}/{name}");
    let url = format!("{device_url}/files/upload?path={device_path}");
    eprintln!(
        "Uploading {} bytes from {} to {} because {}",
        contents.len(),
        host_path,
        device_path,
        reason
    );
    if dry_run {
        return Ok(());
    }
    client
        .post(url)
        .bearer_auth(bearer_token)
        .body(contents)
        .send()
        .await?;
    Ok(())
}

async fn list_host_dir(host_dir: &str) -> Result<HashMap<String, FileAttributes>, anyhow::Error> {
    let mut entries = HashMap::new();
    for file in std::fs::read_dir(host_dir)? {
        let file = file?;
        let metadata = file.metadata()?;
        let is_dir = metadata.is_dir();
        let size = metadata.len();
        let file_name = file
            .path()
            .file_name()
            .unwrap()
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("non UTF-8 file name"))?
            .to_owned();

        entries.insert(file_name, FileAttributes { size, is_dir });
    }
    Ok(entries)
}

// Return Map from name to file size
async fn list_remote_dir(
    device_url: &str,
    path: &str,
    bearer_token: &str,
    client: &Client,
) -> Result<HashMap<String, FileAttributes>, anyhow::Error> {
    let url = format!("{device_url}/files/read-dir?path={path}");
    let resp = client
        .get(&url)
        .bearer_auth(bearer_token)
        .send()
        .await
        .context("read-dir request failed")?;
    let mut entries = HashMap::new();
    match resp.status().as_u16() {
        404 => return Ok(entries),
        200 => (),
        code => anyhow::bail!("read-dir returned status code of {code}: {url}"),
    }
    let text = resp.text().await.context("failed to retrieve text")?;
    for line in text.lines() {
        if line.starts_with("total") {
            break;
        }
        let (name, attrs) = process_read_dir_line(&line)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse read-dir line: {}", line))?;
        entries.insert(name, attrs);
    }
    Ok(entries)
}

fn process_read_dir_line(line: &str) -> Option<(String, FileAttributes)> {
    let mut args = line.split_ascii_whitespace();
    let is_dir = args.next()? == "d";
    let size: u64 = args.next()?.parse().ok()?;
    let _ts = args.next()?;
    let path = args.next()?;
    Some((path.to_owned(), FileAttributes { size, is_dir }))
}

async fn remove_file(
    device_url: &str,
    path: &str,
    bearer_token: &str,
    client: &Client,
) -> Result<(), anyhow::Error> {
    let url = format!("{device_url}/files/remove-file?path={path}");
    let resp = client
        .post(url)
        .bearer_auth(bearer_token)
        .body("")
        .send()
        .await
        .context("read-dir request failed")?;
    match resp.status().as_u16() {
        200 => (),
        code => anyhow::bail!("remove-file returned status code of {code}"),
    }
    Ok(())
}

fn new_client() -> Client {
    ClientBuilder::new()
        .danger_accept_invalid_certs(true)
        .build()
        .unwrap()
}
