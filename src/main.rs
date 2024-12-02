use anyhow::Result;
use axum::{
    extract::Path,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use clap::{Parser, Subcommand};
use futures::TryStreamExt;
use nix_compat::nar::reader::r#async as nar_reader;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{self, AsyncRead, AsyncReadExt};
use tokio::sync::mpsc;
use tracing::{debug, error, info, instrument};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

static BUFFER_SIZE: usize = 8192;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    Serve { store_uri: String },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(fmt::layer())
        .with(EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();

    match cli.command {
        Command::Serve { store_uri } => serve(store_uri).await,
    }
}

#[instrument]
async fn serve(store_uri: String) -> Result<()> {
    let addr = SocketAddr::from(([127, 0, 0, 1], 8080));
    let store_uri = Arc::new(store_uri);

    let app = Router::new()
        .route("/*path", get(handle_request))
        .with_state(store_uri);

    info!("Listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();

    Ok(())
}

#[instrument]
async fn handle_request(
    Path(path): Path<String>,
    axum::extract::State(store_uri): axum::extract::State<Arc<String>>,
) -> impl IntoResponse {
    match NixStorePath::parse(&path) {
        None => (StatusCode::NOT_FOUND, "Not found").into_response(),
        Some(store_path) => {
            let uri = format!("{}/{}.narinfo", store_uri, store_path.hash);
            info!("Fetching narinfo from {}", uri);
            let raw_narinfo = reqwest::get(uri).await.unwrap().text().await.unwrap();
            let narinfo = nix_compat::narinfo::NarInfo::parse(&raw_narinfo).unwrap();

            let nar_path = narinfo.url;
            let nar_url = format!("{}/{}", store_uri, nar_path);
            info!("Redirecting to {}", nar_url);

            let client = reqwest::Client::new();
            let nar_resp = match client.get(&nar_url).send().await {
                Ok(resp) => resp,
                Err(_) => {
                    return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to fetch NAR")
                        .into_response()
                }
            };

            if !nar_resp.status().is_success() {
                return (StatusCode::BAD_GATEWAY, "Failed to fetch NAR").into_response();
            }

            let s = nar_resp.bytes_stream().map_err(|e| {
                let e = e.without_url();
                error!(e=%e, "Failed to get NAR body");
                io::Error::new(io::ErrorKind::BrokenPipe, e.to_string())
            });
            let r = tokio_util::io::StreamReader::new(s);

            let r: Box<dyn AsyncRead + Send + Unpin> = match narinfo.compression {
                None => Box::new(r),
                Some("bzip2") => Box::new(async_compression::tokio::bufread::BzDecoder::new(r)),
                Some("gzip") => Box::new(async_compression::tokio::bufread::GzipDecoder::new(r)),
                Some("xz") => Box::new(async_compression::tokio::bufread::XzDecoder::new(r)),
                Some("zstd") => Box::new(async_compression::tokio::bufread::ZstdDecoder::new(r)),
                Some(comp_str) => {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("Unsupported compression: {comp_str}"),
                    )
                        .into_response();
                }
            };

            let mut r = io::BufReader::new(r);

            let (tx, rx) = mpsc::channel(BUFFER_SIZE);

            let stream = tokio_stream::wrappers::ReceiverStream::new(rx);

            let target_path = store_path
                .file_path
                .map(|s| format!("/{}", s))
                .unwrap_or("/".to_string());

            info!("Searching for: {:?}", target_path);

            tokio::spawn(async move {
                let root_node = nar_reader::open(&mut r).await.unwrap();

                if let Err(err) = search_nar(root_node, target_path, tx).await {
                    error!(e=%err, "Failed to search NAR");
                }
            });

            info!("Streaming response");

            Response::builder()
                .status(StatusCode::OK)
                .body(axum::body::Body::from_stream(stream))
                .unwrap()
        }
    }
}

// TODO: support symlinks pointing to other NARs
// Support directories
#[instrument(skip(node, tx))]
async fn search_nar<'a, 'r: 'a>(
    node: nar_reader::Node<'a, 'r>,
    target_path: String,
    tx: mpsc::Sender<std::result::Result<Vec<u8>, std::io::Error>>,
) -> Result<()> {
    Ok(match node {
        nar_reader::Node::File { reader, .. } => {
            stream_file(reader, tx.clone(), true).await;
        }
        nar_reader::Node::Directory(mut dir_reader) => {
            let (dir_name, remaining_path) = match target_path.split_once('/') {
                Some((dir, rest)) => (dir.to_string(), rest.to_string()),
                None => (target_path, String::new()),
            };

            debug!("Searching directory: {}", dir_name);

            while let Some(entry) = dir_reader.next().await? {
                debug!("Entry: {:?}", std::str::from_utf8(&entry.name).unwrap());
                match entry.node {
                    nar_reader::Node::File { reader, .. } => {
                        stream_file(reader, tx.clone(), entry.name == remaining_path.as_bytes())
                            .await;
                    }
                    nar_reader::Node::Directory(_) => {
                        Box::pin(search_nar(
                            entry.node,
                            remaining_path.to_string(),
                            tx.clone(),
                        ))
                        .await?;
                    }
                    _ => (),
                }
            }
        }
        _ => (),
    })
}

#[instrument(skip(reader, tx, should_stream))]
async fn stream_file(
    mut reader: nar_reader::FileReader<'_, '_>,
    tx: mpsc::Sender<std::result::Result<Vec<u8>, std::io::Error>>,
    should_stream: bool,
) {
    let mut buffer = vec![0u8; BUFFER_SIZE];

    loop {
        match reader.read(&mut buffer).await {
            Ok(0) => break,
            Ok(n) => {
                if should_stream {
                    if tx.send(Ok(buffer[..n].to_vec())).await.is_err() {
                        break;
                    }
                }
            }
            Err(e) => {
                error!(e=%e, "Failed to read from file");
                break;
            }
        }
    }
}

struct NixStorePath {
    hash: String,
    file_path: Option<String>,
}

use nom::{
    branch::alt,
    bytes::complete::{tag, take, take_while1},
    character::complete::char,
    combinator::{opt, recognize, rest},
    sequence::{pair, preceded, tuple},
    IResult,
};

fn parse_nix_store_path(input: &str) -> IResult<&str, NixStorePath> {
    alt((parse_full_nix_store_path, parse_hash_only_path))(input)
}

fn parse_full_nix_store_path(input: &str) -> IResult<&str, NixStorePath> {
    let (remaining, (store_path, file_path)) = tuple((
        recognize(pair(
            opt(char('/')),
            pair(tag("nix/store/"), take_while1(|c: char| c != '/')),
        )),
        opt(preceded(char('/'), rest)),
    ))(input)?;

    let hash = store_path
        .trim_start_matches('/')
        .trim_start_matches("nix/store/")
        .get(..32)
        .unwrap_or_default()
        .to_string();

    Ok((
        remaining,
        NixStorePath {
            hash: hash.to_string(),
            file_path: file_path.map(|s| s.trim_end_matches("/").to_string()),
        },
    ))
}

fn parse_hash_only_path(input: &str) -> IResult<&str, NixStorePath> {
    let (remaining, (hash, file_path)) =
        tuple((take(32usize), opt(preceded(char('/'), rest))))(input)?;

    Ok((
        remaining,
        NixStorePath {
            hash: hash.to_string(),
            file_path: file_path.map(|s| s.trim_end_matches("/").to_string()),
        },
    ))
}

impl<'a> NixStorePath {
    fn parse(path: &'a str) -> Option<Self> {
        match parse_nix_store_path(path) {
            Ok((_, nix_path)) => Some(nix_path),
            Err(err) => {
                eprintln!("Failed to parse Nix store path: {:?}", err);
                None
            }
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_parse_store_path() {
        let path = "/nix/store/8h6x8md74j4b4xcy4xd9y4cc210hhaxx-foo";
        let nix_path = NixStorePath::parse(path).unwrap();
        assert_eq!(nix_path.hash, "8h6x8md74j4b4xcy4xd9y4cc210hhaxx");
        assert_eq!(nix_path.file_path, None);
    }

    #[test]
    fn test_parse_store_path_with_file_path() {
        let path = "nix/store/8h6x8md74j4b4xcy4xd9y4cc210hhaxx-foo/bin/foo";
        let nix_path = NixStorePath::parse(path).unwrap();
        assert_eq!(nix_path.hash, "8h6x8md74j4b4xcy4xd9y4cc210hhaxx");
        assert_eq!(nix_path.file_path, Some("bin/foo".to_string()));
    }

    #[test]
    fn test_parse_store_path_nar_serve() {
        let path =
            "zhpwxx771lz7hdyiv9f611w80wja0vsn/nix-2.26.0pre19700101_838d3c1-aarch64-darwin.tar.xz";
        let nix_path = NixStorePath::parse(path).unwrap();
        assert_eq!(nix_path.hash, "zhpwxx771lz7hdyiv9f611w80wja0vsn");
        assert_eq!(
            nix_path.file_path,
            Some("nix-2.26.0pre19700101_838d3c1-aarch64-darwin.tar.xz".to_string())
        );
    }
}
