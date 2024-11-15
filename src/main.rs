use anyhow::{Context, Result};
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Client, Request, Response, Server};
use clap::{Subcommand, Parser};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command
}

#[derive(Subcommand, Debug)]
enum Command {
    Serve {
        store_uri: String,
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Serve { store_uri } => serve(store_uri).await
    }
}

async fn serve(store_uri: String) -> Result<()> {
    let addr = SocketAddr::from(([127, 0, 0, 1], 8080));
    let store_uri = Arc::new(store_uri);

    let make_svc = make_service_fn(move |_conn| {
        let store_uri = Arc::clone(&store_uri);
        async move {
            Ok::<_, Infallible>(service_fn(move |req| {
                let store_uri = Arc::clone(&store_uri);
                let (parts, _body) = req.into_parts();
                let uri = parts.uri.clone();
                async move {
                    handle_request(store_uri, uri).await
                }
            }))
        }
    });

    let server = Server::bind(&addr).serve(make_svc);

    eprintln!("Listening on http://{}", addr);

    server.await.context("Server error")?;

    Ok(())
}

async fn handle_request(
    store_uri: Arc<String>,
    uri: hyper::Uri,
) -> Result<Response<Body>, Infallible> {
    if let Some(store_path) = NixStorePath::parse(uri.path()) {
        let narinfo = fetch_narinfo(store_uri, &store_path.hash).await.unwrap();
        let nar_url = narinfo.borrow_narinfo().url;
        eprintln!("Redirecting to {}", nar_url);
        Ok(Response::builder().status(200).body(Body::from(format!("{}",narinfo.borrow_narinfo()))).unwrap())
    } else {
        Ok(Response::builder()
            .status(404)
            .body(Body::from("Not Found"))
            .unwrap())
    }
}

struct NixStorePath {
    full_path: String,
    store_path: String,
    hash: String,
    file_path: Option<String>
}

use nom::{
    bytes::complete::{tag, take, take_while1},
    character::complete::char,
    combinator::{opt, rest},
    sequence::{preceded, tuple},
    IResult,
};

fn parse_nix_store_path(input: &str) -> IResult<&str, NixStorePath> {
    let (remaining, (_, store_path, file_path)) = tuple((
        tag("/nix/store/"),
        take_while1(|c: char| c != '/'),
        opt(preceded(char('/'), rest)),
    ))(input)?;

    let (_, hash) = take(32u8)(store_path)?;

    Ok((
        remaining,
        NixStorePath {
            full_path: input.to_string(),
            store_path: store_path.to_string(),
            hash: hash.to_string(),
            file_path: file_path.map(|s| s.to_string()),
        },
    ))
}

impl<'a> NixStorePath {
    fn parse(path: &'a str) -> Option<Self> {
        match parse_nix_store_path(&path) {
            Ok((_, nix_path)) => Some(nix_path),
            Err(_) => None,
        }
    }
}

use ouroboros::self_referencing;
#[self_referencing]
struct OwnedNarInfo {
    raw: String,
    #[borrows(raw)]
    #[covariant]
    narinfo: nix_compat::narinfo::NarInfo<'this>,
}

async fn fetch_narinfo<'a>(store_uri: Arc<String>, hash: &str) -> Result<OwnedNarInfo> {
    let uri = format!("{}/{}.narinfo", store_uri, hash);
    eprintln!("Fetching narinfo from {}", uri);
    let narinfo = reqwest::get(uri)
        .await?
        .text()
        .await
        .map(|raw| OwnedNarInfoBuilder {
            raw,
            narinfo_builder: |raw: &String| {
                nix_compat::narinfo::NarInfo::parse(raw).unwrap()
            },
        }.build())?;
    Ok(narinfo)
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_parse_store_path() {
        let path = "/nix/store/8h6x8md74j4b4xcy4xd9y4cc210hhaxx-foo";
        let nix_path = parse_nix_store_path(path).unwrap().1;
        assert_eq!(nix_path.full_path, path);
        assert_eq!(nix_path.store_path, "8h6x8md74j4b4xcy4xd9y4cc210hhaxx-foo");
        assert_eq!(nix_path.hash, "8h6x8md74j4b4xcy4xd9y4cc210hhaxx");
        assert_eq!(nix_path.file_path, None);
    }

    #[test]
    fn test_parse_store_path_with_file_path() {
        let path = "/nix/store/8h6x8md74j4b4xcy4xd9y4cc210hhaxx-foo/bin/foo";
        let nix_path = parse_nix_store_path(path).unwrap().1;
        assert_eq!(nix_path.full_path, path);
        assert_eq!(nix_path.store_path, "8h6x8md74j4b4xcy4xd9y4cc210hhaxx-foo");
        assert_eq!(nix_path.hash, "8h6x8md74j4b4xcy4xd9y4cc210hhaxx");
        assert_eq!(nix_path.file_path, Some("bin/foo".to_string()));
    }
}
