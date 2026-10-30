// SPDX-License-Identifier: AGPL-3.0-or-later

use std::fmt;
use std::fs::Metadata;
use std::io;
use std::path::{Component, Path, PathBuf};

use percent_encoding::percent_decode_str;
use tauri::http::{Method, Request, Response, StatusCode, header};
use tokio::io::AsyncReadExt as _;

use super::csp::build_csp;
use super::index_html::{IndexHtmlInput, random_csp_nonce, rewrite_index_html};
use super::mime::{cache_control, content_type, is_served_extension};
use super::static_metadata::{rewrite, should_rewrite};
use super::url::parse_local_app_url;
use crate::build_channel::BuildChannel;

const INDEX_HTML: &str = "index.html";
const LOCAL_APP_REWRITTEN_FILE_MAX_BYTES: u64 = 4 * 1024 * 1024;
const TEXT_CONTENT_TYPE: &str = "text/plain; charset=utf-8";
const ALLOWED_METHODS: &str = "GET, HEAD";
const NOT_FOUND_TEXT: &str = "Not found";
const METHOD_NOT_ALLOWED_TEXT: &str = "Method not allowed";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockReason {
    InvalidOrigin,
    InvalidPath,
    PathEscape,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalAppFile {
    pub path: PathBuf,
    pub size: u64,
    pub cache_control: &'static str,
    pub content_type: &'static str,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Resolved {
    File(LocalAppFile),
    NotFound,
    Blocked(BlockReason),
}

#[derive(Debug)]
pub enum FilesError {
    MissingIndex(PathBuf),
    Io(io::Error),
    ChangedDuringOpen,
    TooLarge { path: PathBuf, max: u64 },
}

impl fmt::Display for FilesError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingIndex(root) => {
                write!(
                    formatter,
                    "Local renderer root is missing {INDEX_HTML}: {}",
                    root.display()
                )
            }
            Self::Io(error) => write!(formatter, "Local renderer file I/O failed: {error}"),
            Self::ChangedDuringOpen => {
                formatter.write_str("Local renderer file changed while it was being opened")
            }
            Self::TooLarge { path, max } => {
                write!(
                    formatter,
                    "Local renderer file exceeds {max} bytes: {}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for FilesError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for FilesError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub struct LocalAppFiles {
    root: PathBuf,
}

impl LocalAppFiles {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub async fn resolve(&self, request_url: &str) -> Result<Resolved, FilesError> {
        let Some(parsed) = parse_local_app_url(request_url) else {
            return Ok(Resolved::Blocked(BlockReason::InvalidOrigin));
        };
        let Some(relative_path) = safe_relative_path(parsed.path()) else {
            return Ok(Resolved::Blocked(BlockReason::InvalidPath));
        };
        let index_path = self.root.join(INDEX_HTML);
        if stat_file(&index_path).await?.is_none() {
            return Err(FilesError::MissingIndex(self.root.clone()));
        }
        let requested_path = join_relative(&self.root, &relative_path);
        if !requested_path.starts_with(&self.root) {
            return Ok(Resolved::Blocked(BlockReason::PathEscape));
        }
        if let Some(metadata) = stat_file(&requested_path).await? {
            return Ok(Resolved::File(describe_file(
                requested_path,
                metadata.len(),
            )));
        }
        if is_served_extension(&relative_path) {
            return Ok(Resolved::NotFound);
        }
        match stat_file(&index_path).await? {
            Some(metadata) => Ok(Resolved::File(describe_file(index_path, metadata.len()))),
            None => Err(FilesError::MissingIndex(self.root.clone())),
        }
    }

    pub async fn read(
        &self,
        file: &LocalAppFile,
        max_bytes: Option<u64>,
    ) -> Result<Vec<u8>, FilesError> {
        let mut handle = tokio::fs::File::from_std(open_read_only(&file.path)?);
        let metadata = handle.metadata().await?;
        if !metadata.is_file() || metadata.len() != file.size {
            return Err(FilesError::ChangedDuringOpen);
        }
        if max_bytes.is_some_and(|max| metadata.len() > max) {
            return Err(FilesError::TooLarge {
                path: file.path.clone(),
                max: max_bytes.unwrap_or_default(),
            });
        }
        let mut contents = Vec::with_capacity(usize::try_from(file.size).unwrap_or_default());
        (&mut handle)
            .take(file.size)
            .read_to_end(&mut contents)
            .await?;
        if contents.len() as u64 != file.size {
            return Err(FilesError::ChangedDuringOpen);
        }
        Ok(contents)
    }
}

pub fn is_index_file(path: &Path) -> bool {
    path.file_name().is_some_and(|name| name == INDEX_HTML)
}

pub struct IndexContext {
    pub preboot_theme: Option<String>,
    pub product_name: Option<String>,
    pub cleartext_instance_origins: Vec<String>,
}

fn open_read_only(path: &Path) -> io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
}

fn describe_file(path: PathBuf, size: u64) -> LocalAppFile {
    let text = path.to_string_lossy();
    LocalAppFile {
        size,
        cache_control: cache_control(&text),
        content_type: content_type(&text),
        path,
    }
}

/// `lstat` that yields only regular, non-symlink files; a missing entry is `None`, other errors propagate.
async fn stat_file(path: &Path) -> Result<Option<Metadata>, FilesError> {
    let metadata = match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    Ok((metadata.is_file() && !metadata.file_type().is_symlink()).then_some(metadata))
}

fn join_relative(root: &Path, relative_path: &str) -> PathBuf {
    let mut joined = root.to_path_buf();
    joined.extend(
        relative_path
            .split('/')
            .filter(|segment| !segment.is_empty()),
    );
    joined
}

/// `decodeURIComponent`: every `%` must start a valid escape and the result must be UTF-8.
fn decode_uri_component(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'%'
            && !bytes
                .get(index + 1..index + 3)
                .is_some_and(|digits| digits.iter().all(u8::is_ascii_hexdigit))
        {
            return None;
        }
    }
    percent_decode_str(value)
        .decode_utf8()
        .ok()
        .map(std::borrow::Cow::into_owned)
}

/// Port of `getSafeRelativePath`, with POSIX `path.normalize` semantics (trailing slash preserved).
fn safe_relative_path(raw_pathname: &str) -> Option<String> {
    let decoded = decode_uri_component(raw_pathname)?;
    if decoded.contains('\0') || decoded.contains('\\') {
        return None;
    }
    let stripped = decoded.trim_start_matches('/');
    if stripped.is_empty() {
        return Some(INDEX_HTML.to_owned());
    }
    let mut segments: Vec<&str> = Vec::new();
    for segment in stripped.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                if segments.last().is_some_and(|last| *last != "..") {
                    segments.pop();
                } else {
                    segments.push("..");
                }
            }
            other => segments.push(other),
        }
    }
    let mut normalized = if segments.is_empty() {
        ".".to_owned()
    } else {
        segments.join("/")
    };
    if stripped.ends_with('/') && normalized != "." {
        normalized.push('/');
    }
    if normalized == "." || normalized == ".." || normalized.starts_with("../") {
        return None;
    }
    Path::new(&normalized)
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
        .then_some(normalized)
}

fn text_response(status: StatusCode, text: String) -> Response<Vec<u8>> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, TEXT_CONTENT_TYPE)
        .body(text.into_bytes())
        .expect("static response parts are valid")
}

fn not_found_response() -> Response<Vec<u8>> {
    text_response(StatusCode::NOT_FOUND, NOT_FOUND_TEXT.to_owned())
}

fn renderer_unavailable_text() -> String {
    format!(
        "{} could not load its app files. Quit and open it again.",
        BuildChannel::current().app_name()
    )
}

fn renderer_file_unavailable_text() -> String {
    format!(
        "{} could not read one of its app files. Quit and open it again.",
        BuildChannel::current().app_name()
    )
}

pub async fn handle_file_request(
    files: &LocalAppFiles,
    index_context: &IndexContext,
    request: &Request<Vec<u8>>,
) -> Response<Vec<u8>> {
    if request.method() != Method::GET && request.method() != Method::HEAD {
        return Response::builder()
            .status(StatusCode::METHOD_NOT_ALLOWED)
            .header(header::ALLOW, ALLOWED_METHODS)
            .header(header::CONTENT_TYPE, TEXT_CONTENT_TYPE)
            .body(METHOD_NOT_ALLOWED_TEXT.as_bytes().to_vec())
            .expect("static response parts are valid");
    }
    let resolved = match files.resolve(&request.uri().to_string()).await {
        Ok(resolved) => resolved,
        Err(error) => {
            tracing::error!(%error, "Failed to resolve a local app renderer request");
            return text_response(StatusCode::SERVICE_UNAVAILABLE, renderer_unavailable_text());
        }
    };
    match resolved {
        Resolved::Blocked(reason) => {
            tracing::warn!(?reason, "Rejected a local app renderer request");
            not_found_response()
        }
        Resolved::NotFound => not_found_response(),
        Resolved::File(file) => match serve_file(files, index_context, request, &file).await {
            Ok(response) => response,
            Err(error) => {
                tracing::error!(%error, "Failed to read a local app renderer file");
                text_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    renderer_file_unavailable_text(),
                )
            }
        },
    }
}

async fn serve_file(
    files: &LocalAppFiles,
    index_context: &IndexContext,
    request: &Request<Vec<u8>>,
    file: &LocalAppFile,
) -> Result<Response<Vec<u8>>, FilesError> {
    let file_name = file
        .path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let static_metadata = should_rewrite(file_name);
    let index = is_index_file(&file.path);
    let builder = Response::builder()
        .status(StatusCode::OK)
        .header(header::CACHE_CONTROL, file.cache_control)
        .header(header::CONTENT_TYPE, file.content_type);
    let head = request.method() == Method::HEAD;
    let (builder, content_length, body) = if static_metadata || index {
        let contents = files
            .read(file, Some(LOCAL_APP_REWRITTEN_FILE_MAX_BYTES))
            .await?;
        let text = String::from_utf8_lossy(&contents);
        let (builder, rewritten) = if index {
            let nonce = random_csp_nonce();
            let html = rewrite_index_html(&IndexHtmlInput {
                html: &text,
                nonce: &nonce,
                preboot_theme: index_context.preboot_theme.as_deref(),
                product_name: index_context.product_name.as_deref(),
            });
            let csp = build_csp(&nonce, &index_context.cleartext_instance_origins);
            (builder.header(header::CONTENT_SECURITY_POLICY, csp), html)
        } else {
            let rewritten = rewrite(file_name, &text).ok_or(FilesError::ChangedDuringOpen)?;
            (builder, rewritten)
        };
        let length = rewritten.len() as u64;
        if length > LOCAL_APP_REWRITTEN_FILE_MAX_BYTES {
            return Err(FilesError::TooLarge {
                path: file.path.clone(),
                max: LOCAL_APP_REWRITTEN_FILE_MAX_BYTES,
            });
        }
        (builder, length, rewritten.into_bytes())
    } else if head {
        (builder, file.size, Vec::new())
    } else {
        (builder, file.size, files.read(file, None).await?)
    };
    Ok(builder
        .header(header::CONTENT_LENGTH, content_length)
        .body(if head { Vec::new() } else { body })
        .expect("response parts are valid"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    const INDEX_SOURCE: &str = r#"<html><head><title>x</title><script nonce="{{CSP_NONCE_PLACEHOLDER}}"></script></head></html>"#;

    fn fixture() -> (TempDir, LocalAppFiles) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("index.html"), INDEX_SOURCE).unwrap();
        std::fs::create_dir(dir.path().join("assets")).unwrap();
        std::fs::write(
            dir.path().join("assets/app-abcdef1234.js"),
            "console.log(1)",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("manifest.json"),
            r#"{"icon":"https://cdn.test/web/i.png","x":"{{STATIC_CDN_ENDPOINT}}/y"}"#,
        )
        .unwrap();
        let files = LocalAppFiles::new(dir.path().to_path_buf());
        (dir, files)
    }

    fn context() -> IndexContext {
        IndexContext {
            preboot_theme: Some("dark".to_owned()),
            product_name: None,
            cleartext_instance_origins: Vec::new(),
        }
    }

    fn request(method: Method, path: &str) -> Request<Vec<u8>> {
        Request::builder()
            .method(method)
            .uri(format!("{}{path}", super::super::url::origin()))
            .body(Vec::new())
            .unwrap()
    }

    fn header_text(response: &Response<Vec<u8>>, name: header::HeaderName) -> &str {
        response.headers().get(name).unwrap().to_str().unwrap()
    }

    fn url(path: &str) -> String {
        format!("{}{path}", super::super::url::origin())
    }

    #[test]
    fn safe_relative_path_matches_node_semantics() {
        assert_eq!(safe_relative_path("/").as_deref(), Some("index.html"));
        assert_eq!(safe_relative_path("///").as_deref(), Some("index.html"));
        assert_eq!(safe_relative_path("/a/b.js").as_deref(), Some("a/b.js"));
        assert_eq!(safe_relative_path("//a//b/./c").as_deref(), Some("a/b/c"));
        assert_eq!(safe_relative_path("/a/../b").as_deref(), Some("b"));
        assert_eq!(safe_relative_path("/a/").as_deref(), Some("a/"));
        assert_eq!(safe_relative_path("/%61").as_deref(), Some("a"));
        assert_eq!(safe_relative_path("/\u{e9}").as_deref(), Some("\u{e9}"));
        for rejected in [
            "/..",
            "/../x",
            "/a/../../x",
            "/%2e%2e/x",
            "/..%2fx",
            "/%2e%2e%2f",
            "/a%5cb",
            "/a%00b",
            "/.",
            "/./",
            "/a/..",
            "/%",
            "/%zz",
            "/%f0",
            "/%c0%af",
            "/%e0%80%80",
            "/%ed%a0%80",
        ] {
            assert_eq!(safe_relative_path(rejected), None, "{rejected}");
        }
    }

    #[tokio::test]
    async fn resolve_blocks_foreign_origin_and_traversal() {
        let (_dir, files) = fixture();
        assert_eq!(
            files.resolve("https://example.com/").await.unwrap(),
            Resolved::Blocked(BlockReason::InvalidOrigin)
        );
        for path in [
            "/..%2fsecret",
            "/%2e%2e%2fsecret",
            "/a%5c..%5csecret",
            "/a%00b",
            "/..%2f..%2fetc%2fpasswd",
            "/%zz",
        ] {
            assert_eq!(
                files.resolve(&url(path)).await.unwrap(),
                Resolved::Blocked(BlockReason::InvalidPath),
                "{path}"
            );
        }
    }

    #[tokio::test]
    async fn resolve_serves_files_with_metadata() {
        let (dir, files) = fixture();
        let Resolved::File(file) = files
            .resolve(&url("/assets/app-abcdef1234.js"))
            .await
            .unwrap()
        else {
            panic!("expected a file");
        };
        assert_eq!(file.path, dir.path().join("assets/app-abcdef1234.js"));
        assert_eq!(file.size, 14);
        assert_eq!(file.cache_control, "public, max-age=31536000, immutable");
        assert_eq!(file.content_type, "application/javascript; charset=utf-8");
        let Resolved::File(root_file) = files.resolve(&url("/")).await.unwrap() else {
            panic!("expected index");
        };
        assert_eq!(root_file.path, dir.path().join("index.html"));
        assert_eq!(root_file.cache_control, "no-store");
    }

    #[tokio::test]
    async fn resolve_falls_back_to_index_only_for_extensionless_routes() {
        let (dir, files) = fixture();
        for path in ["/channels/1", "/assets", "/assets/", "/a.unknown"] {
            let Resolved::File(file) = files.resolve(&url(path)).await.unwrap() else {
                panic!("{path} should fall back to index");
            };
            assert_eq!(file.path, dir.path().join("index.html"), "{path}");
        }
        for path in ["/missing.js", "/assets/missing.png", "/model.onnx"] {
            assert_eq!(
                files.resolve(&url(path)).await.unwrap(),
                Resolved::NotFound,
                "{path}"
            );
        }
    }

    #[tokio::test]
    async fn missing_index_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let files = LocalAppFiles::new(dir.path().to_path_buf());
        let error = files.resolve(&url("/")).await.unwrap_err();
        assert!(matches!(error, FilesError::MissingIndex(ref root) if root == dir.path()));
        assert!(error.to_string().contains("index.html"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinks_are_never_served() {
        let (dir, files) = fixture();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "secret").unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("secret.txt"),
            dir.path().join("link.txt"),
        )
        .unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("linkdir")).unwrap();
        assert_eq!(
            files.resolve(&url("/link.txt")).await.unwrap(),
            Resolved::NotFound
        );
        let Resolved::File(file) = files.resolve(&url("/linkdir/secret")).await.unwrap() else {
            panic!("extensionless path through a symlinked directory is only the SPA fallback");
        };
        assert_eq!(file.path, dir.path().join("index.html"));

        let response =
            handle_file_request(&files, &context(), &request(Method::GET, "/link.txt")).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn read_refuses_symlink_and_changed_files() {
        let (dir, files) = fixture();
        let target = dir.path().join("assets/app-abcdef1234.js");
        let link = dir.path().join("swapped.js");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let described = describe_file(link, 14);
        assert!(matches!(
            files.read(&described, None).await,
            Err(FilesError::Io(_))
        ));

        let stale = describe_file(target.clone(), 3);
        assert!(matches!(
            files.read(&stale, None).await,
            Err(FilesError::ChangedDuringOpen)
        ));

        let exact = describe_file(target.clone(), 14);
        assert_eq!(
            files.read(&exact, Some(14)).await.unwrap(),
            b"console.log(1)"
        );
        assert!(matches!(
            files.read(&exact, Some(13)).await,
            Err(FilesError::TooLarge { max: 13, .. })
        ));
        let directory = describe_file(dir.path().join("assets"), 14);
        assert!(matches!(
            files.read(&directory, None).await,
            Err(FilesError::ChangedDuringOpen)
        ));
    }

    #[test]
    fn index_file_detection_uses_basename() {
        assert!(is_index_file(Path::new("/a/index.html")));
        assert!(!is_index_file(Path::new("/a/index.html/b")));
        assert!(!is_index_file(Path::new("/a/xindex.html")));
    }

    #[tokio::test]
    async fn index_gets_csp_header_and_nonce() {
        let (_dir, files) = fixture();
        let response =
            handle_file_request(&files, &context(), &request(Method::GET, "/channels/1")).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(header_text(&response, header::CACHE_CONTROL), "no-store");
        assert_eq!(
            header_text(&response, header::CONTENT_TYPE),
            "text/html; charset=utf-8"
        );
        let csp = header_text(&response, header::CONTENT_SECURITY_POLICY);
        let body = String::from_utf8(response.body().clone()).unwrap();
        assert!(!body.contains("{{CSP_NONCE_PLACEHOLDER}}"));
        let nonce = body
            .split("nonce=\"")
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap();
        assert_eq!(nonce.len(), 32);
        assert!(csp.contains(&format!("'nonce-{nonce}'")));
        assert!(body.starts_with("<html class=\"theme-dark\">"));
        assert_eq!(
            header_text(&response, header::CONTENT_LENGTH),
            body.len().to_string()
        );
    }

    #[tokio::test]
    async fn head_reports_get_length_with_empty_body() {
        let (_dir, files) = fixture();
        for path in ["/assets/app-abcdef1234.js", "/", "/manifest.json"] {
            let get = handle_file_request(&files, &context(), &request(Method::GET, path)).await;
            let head = handle_file_request(&files, &context(), &request(Method::HEAD, path)).await;
            assert_eq!(head.status(), StatusCode::OK, "{path}");
            assert!(head.body().is_empty(), "{path}");
            assert_eq!(
                header_text(&head, header::CONTENT_LENGTH),
                get.body().len().to_string(),
                "{path}"
            );
            assert_eq!(
                header_text(&head, header::CONTENT_TYPE),
                header_text(&get, header::CONTENT_TYPE)
            );
        }
    }

    #[tokio::test]
    async fn static_files_are_served_verbatim_and_metadata_rewritten() {
        let (_dir, files) = fixture();
        let asset = handle_file_request(
            &files,
            &context(),
            &request(Method::GET, "/assets/app-abcdef1234.js"),
        )
        .await;
        assert_eq!(asset.body(), b"console.log(1)");
        assert_eq!(
            header_text(&asset, header::CACHE_CONTROL),
            "public, max-age=31536000, immutable"
        );
        assert!(
            asset
                .headers()
                .get(header::CONTENT_SECURITY_POLICY)
                .is_none()
        );
        let manifest =
            handle_file_request(&files, &context(), &request(Method::GET, "/manifest.json")).await;
        assert_eq!(
            String::from_utf8(manifest.body().clone()).unwrap(),
            r#"{"icon":"/web/i.png","x":"/y"}"#
        );
        assert_eq!(header_text(&manifest, header::CACHE_CONTROL), "no-store");
        assert!(
            manifest
                .headers()
                .get(header::CONTENT_SECURITY_POLICY)
                .is_none()
        );
    }

    #[tokio::test]
    async fn missing_asset_is_plain_404() {
        let (_dir, files) = fixture();
        let response = handle_file_request(
            &files,
            &context(),
            &request(Method::GET, "/assets/missing.js"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(response.body(), b"Not found");
        assert_eq!(
            header_text(&response, header::CONTENT_TYPE),
            TEXT_CONTENT_TYPE
        );
    }

    #[tokio::test]
    async fn traversal_requests_are_404() {
        let (_dir, files) = fixture();
        for path in ["/..%2fx", "/%2e%2e%2fx", "/a%5cb", "/a%00b"] {
            let response =
                handle_file_request(&files, &context(), &request(Method::GET, path)).await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        }
    }

    #[tokio::test]
    async fn non_get_head_methods_get_405() {
        let (_dir, files) = fixture();
        let response = handle_file_request(&files, &context(), &request(Method::POST, "/")).await;
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(header_text(&response, header::ALLOW), "GET, HEAD");
        assert_eq!(
            header_text(&response, header::CONTENT_TYPE),
            TEXT_CONTENT_TYPE
        );
        assert_eq!(response.body(), b"Method not allowed");
    }

    #[tokio::test]
    async fn missing_index_is_503_and_unreadable_file_is_500() {
        let dir = tempfile::tempdir().unwrap();
        let files = LocalAppFiles::new(dir.path().to_path_buf());
        let response = handle_file_request(&files, &context(), &request(Method::GET, "/")).await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let expected = format!(
            "{} could not load its app files. Quit and open it again.",
            BuildChannel::current().app_name()
        );
        assert_eq!(response.body(), expected.as_bytes());

        std::fs::write(dir.path().join("index.html"), INDEX_SOURCE).unwrap();
        std::fs::write(dir.path().join("big.js"), vec![b'a'; 8]).unwrap();
        let file = describe_file(dir.path().join("big.js"), 8);
        std::fs::write(dir.path().join("big.js"), vec![b'a'; 9]).unwrap();
        let error = files.read(&file, None).await.unwrap_err();
        assert!(matches!(error, FilesError::ChangedDuringOpen));
    }

    #[tokio::test]
    async fn oversized_index_is_500() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("index.html"),
            vec![b'a'; (LOCAL_APP_REWRITTEN_FILE_MAX_BYTES + 1) as usize],
        )
        .unwrap();
        let files = LocalAppFiles::new(dir.path().to_path_buf());
        let response = handle_file_request(&files, &context(), &request(Method::GET, "/")).await;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let expected = format!(
            "{} could not read one of its app files. Quit and open it again.",
            BuildChannel::current().app_name()
        );
        assert_eq!(response.body(), expected.as_bytes());
    }
}
