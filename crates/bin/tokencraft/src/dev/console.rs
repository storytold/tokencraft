use std::{
    collections::HashSet,
    fs,
    io::Read,
    net::Ipv4Addr,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, bail, ensure};
use axum::{
    Router,
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header},
    response::{IntoResponse, Response},
};
use percent_encoding::percent_decode_str;
use tokio::net::TcpListener;
use tokio_util::io::ReaderStream;

pub(super) struct Console {
    listener: TcpListener,
    files: Arc<Files>,
}

struct Files {
    root: PathBuf,
    allowed: HashSet<PathBuf>,
    index: Bytes,
    host: HeaderValue,
}

impl Console {
    pub(super) async fn prepare(directory: &Path, web_port: u16, api_port: u16) -> Result<Self> {
        let root = directory.canonicalize().context("resolve the compiled console directory")?;
        let mut allowed = HashSet::new();
        collect_files(&root, Path::new(""), &mut allowed)?;
        ensure!(
            allowed.contains(Path::new("index.html")) && allowed.iter().any(|file| {
                file.starts_with("assets") && file.extension().is_some_and(|extension| extension == "js")
            }),
            "the console requires a compiled frontend with index.html and JavaScript assets"
        );
        let mut page = String::new();
        let limit = 1024 * 1024;
        fs::File::open(root.join("index.html"))?.take(limit + 1).read_to_string(&mut page)?;
        ensure!(page.len() as u64 <= limit, "compiled console index exceeds 1 MiB");
        let head = page.find("</head>").context("the compiled console index needs a head element")?;
        ensure!(!page.contains("tokencraft-api-port"), "the launcher sets the console's API port");
        page.insert_str(head, &format!("<meta name=\"tokencraft-api-port\" content=\"{api_port}\">\n"));

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, web_port)).await
            .with_context(|| format!("bind localhost console on port {web_port}"))?;
        Ok(Self {
            listener,
            files: Arc::new(Files { root, allowed, index: Bytes::from(page), host: format!("localhost:{web_port}").parse()? }),
        })
    }

    pub(super) async fn serve(self) -> Result<()> {
        axum::serve(self.listener, Router::new().fallback(serve_file).with_state(self.files)).await?;
        bail!("the local console stopped")
    }
}

fn collect_files(root: &Path, directory: &Path, files: &mut HashSet<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(root.join(directory))? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(text) = name.to_str() else { continue; };
        if text.starts_with('.') || text.contains('\\') || text.chars().any(char::is_control) {
            continue;
        }
        let path = directory.join(name);
        let kind = entry.file_type()?;
        if kind.is_dir() {
            collect_files(root, &path, files)?;
        } else if kind.is_file() {
            files.insert(path);
        }
    }
    Ok(())
}

async fn serve_file(State(files): State<Arc<Files>>, method: Method, uri: Uri, headers: HeaderMap) -> Response {
    let mut hosts = headers.get_all(header::HOST).iter();
    if hosts.next() != Some(&files.host) || hosts.next().is_some() {
        return (StatusCode::BAD_REQUEST, "Open the localhost console URL printed by TokenCraft.").into_response();
    }
    if method != Method::GET && method != Method::HEAD {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let Ok(decoded) = percent_decode_str(uri.path()).decode_utf8() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let path = decoded.trim_start_matches('/').trim_end_matches('/');
    let api_reference = path.starts_with("docs/api/");
    if !path.is_empty() && path.split('/').any(|part| {
        let hidden = part.starts_with('.') && !(api_reference && part == ".well-known");
        part.is_empty() || hidden || part.contains('\\') || part.chars().any(char::is_control)
    }) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let name = Path::new(path);
    let html = headers.get(header::ACCEPT).and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.contains("text/html"));
    let asset = if files.allowed.contains(name) { name.to_owned() } else { name.join("index.html") };
    if asset != Path::new("index.html") && files.allowed.contains(&asset) {
        return match open_asset(&files.root, &asset).await {
            Ok((file, size)) => response(method, Body::from_stream(ReaderStream::new(file)), size,
                mime_guess::from_path(asset).first_or_octet_stream().essence_str(), "no-cache"),
            Err(_) => StatusCode::NOT_FOUND.into_response(),
        };
    }
    let static_path = name.components().next().is_some_and(|prefix| {
        files.allowed.iter().any(|file| file.starts_with(Path::new(prefix.as_os_str())))
    });
    // The documentation views share a prefix with the static OpenAPI document.
    let client_route = !static_path || name.starts_with("docs");
    if asset == Path::new("index.html") || (html && name.extension().is_none() && client_route) {
        return response(method, Body::from(files.index.clone()), files.index.len() as u64, "text/html; charset=utf-8", "no-store");
    }
    StatusCode::NOT_FOUND.into_response()
}

async fn open_asset(root: &Path, relative: &Path) -> Result<(tokio::fs::File, u64)> {
    let path = root.join(relative);
    ensure!(tokio::fs::canonicalize(&path).await? == path, "console assets must stay in the compiled directory");
    let meta = tokio::fs::metadata(&path).await?;
    ensure!(meta.is_file(), "console asset is not a file");
    Ok((tokio::fs::File::open(path).await?, meta.len()))
}

fn response(method: Method, body: Body, size: u64, mime: &str, cache: &'static str) -> Response {
    let mut response = Response::new(if method == Method::HEAD { Body::empty() } else { body });
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, mime.parse().expect("valid asset MIME type"));
    headers.insert(header::CONTENT_LENGTH, size.into());
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    response
}
