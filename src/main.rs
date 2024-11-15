use anyhow::{Context, Result};
use clap::Parser;
use std::fs::File;
use std::io;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    url: String,

    #[arg(short, long, default_value = "downloaded_file")]
    output: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    println!("Downloading from URL: {}", args.url);

    // Send GET request to the URL
    let response = reqwest::get(&args.url)
        .await
        .context("Failed to send GET request")?;

    // Check if the request was successful
    if response.status().is_success() {
        // Create a file to save the downloaded content
        let mut file = File::create(&args.output).context("Failed to create output file")?;

        // Copy the response body to the file
        io::copy(&mut response.bytes().await?.as_ref(), &mut file)
            .context("Failed to write data to file")?;

        println!("File downloaded successfully as: {}", args.output);
    } else {
        anyhow::bail!("Failed to download file. Status: {}", response.status());
    }

    Ok(())
}
