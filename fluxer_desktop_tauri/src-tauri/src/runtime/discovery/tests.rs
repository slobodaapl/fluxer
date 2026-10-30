// SPDX-License-Identifier: AGPL-3.0-or-later

use super::super::test_support::{MemoryMarkers, Reply, TestServer, document, outbound};
use super::*;

const ROOT: &str = "/.well-known/fluxer";
const API: &str = "/api/.well-known/fluxer";

fn discovery() -> (RuntimeDiscovery, Arc<MemoryMarkers>) {
    let markers = Arc::new(MemoryMarkers::default());
    (
        RuntimeDiscovery::new(outbound(), markers.markers()),
        markers,
    )
}

async fn resolve(
    discovery: &RuntimeDiscovery,
    input: &str,
) -> Result<LocalAppRuntimePlan, BridgeError> {
    discovery.resolve(input, &CancellationToken::new()).await
}

#[tokio::test]
async fn a_served_document_becomes_a_registered_remembered_plan() {
    let server = TestServer::start().await;
    server.route(ROOT, Reply::json(&document(&server.base)));
    let (discovery, markers) = discovery();

    let plan = resolve(&discovery, &format!("{}/api/", server.base))
        .await
        .unwrap();
    assert_eq!(plan.instance_key, server.api_endpoint());
    assert_eq!(plan.document, document(&server.base));
    assert_eq!(server.requests(), [ROOT]);
    discovery
        .outbound
        .require_registered_origins(&trusted_http_origins(&plan))
        .unwrap();
    let marker = markers
        .written(&format!("runtime_discovery.v1:{}", server.api_endpoint()))
        .await;
    assert_eq!(
        marker,
        json!({
            "url": format!("{}{ROOT}", server.base),
            "document": document(&server.base),
            "anchorRequirement": "any",
        })
    );
}

#[tokio::test]
async fn json_suffix_media_types_are_json() {
    let server = TestServer::start().await;
    server.route(
        ROOT,
        Reply::Fixed(
            200,
            vec![(
                "Content-Type",
                " Application/Fluxer+JSON ; charset=utf-8".to_owned(),
            )],
            document(&server.base).to_string().into_bytes(),
        ),
    );
    let (discovery, _) = discovery();
    assert!(resolve(&discovery, &server.api_endpoint()).await.is_ok());
}

#[tokio::test]
async fn redirects_are_refused_and_never_followed() {
    let server = TestServer::start().await;
    for path in [ROOT, API] {
        server.route(
            path,
            Reply::Fixed(302, vec![("Location", "/elsewhere".to_owned())], Vec::new()),
        );
    }
    let (discovery, _) = discovery();
    let error = resolve(&discovery, &server.api_endpoint())
        .await
        .unwrap_err();
    // A refused redirect is not an answer without discovery, so the instance counts as
    // unreachable and, with nothing remembered, says so.
    assert_eq!(error.name(), DISCOVERY_UNREACHABLE_ERROR_NAME);
    assert!(error.message().contains(&format!(
        "Instance request to {}{ROOT} was answered with an HTTP 302 redirect, which is never followed",
        server.base
    )));
    assert_eq!(server.requests(), [ROOT, API]);
}

#[tokio::test]
async fn answered_errors_fail_discovery_without_the_unreachable_name() {
    let server = TestServer::start().await;
    let (discovery, _) = discovery();
    let error = resolve(&discovery, &server.api_endpoint())
        .await
        .unwrap_err();
    assert_eq!(error.name(), DISCOVERY_FAILED_ERROR_NAME);
    let base = &server.base;
    assert_eq!(
        error.message(),
        format!(
            "No usable instance discovery document was served for {base}/api: \
             {base}{ROOT} (responded HTTP 404); {base}{API} (responded HTTP 404)"
        )
    );
}

#[tokio::test]
async fn malformed_json_is_an_answer_without_discovery() {
    let server = TestServer::start().await;
    for path in [ROOT, API] {
        server.route(
            path,
            Reply::Fixed(
                200,
                vec![("Content-Type", "application/json".to_owned())],
                b"\xEF\xBB\xBF{}".to_vec(),
            ),
        );
    }
    let (discovery, _) = discovery();
    let error = resolve(&discovery, &server.api_endpoint())
        .await
        .unwrap_err();
    assert_eq!(error.name(), DISCOVERY_FAILED_ERROR_NAME);
    assert!(
        error
            .message()
            .contains("responded with a malformed JSON body")
    );
}

#[tokio::test]
async fn non_json_content_types_count_as_unreachable() {
    let server = TestServer::start().await;
    for path in [ROOT, API] {
        server.route(
            path,
            Reply::Fixed(
                200,
                vec![("Content-Type", "text/html".to_owned())],
                document(&server.base).to_string().into_bytes(),
            ),
        );
    }
    let (discovery, _) = discovery();
    let error = resolve(&discovery, &server.api_endpoint())
        .await
        .unwrap_err();
    assert_eq!(error.name(), DISCOVERY_UNREACHABLE_ERROR_NAME);
    assert!(
        error
            .message()
            .contains("responded without a JSON content type")
    );
}

#[tokio::test]
async fn bodies_over_one_mebibyte_are_refused() {
    let server = TestServer::start().await;
    let declared = Reply::Fixed(
        200,
        vec![
            ("Content-Type", "application/json".to_owned()),
            ("Content-Length", "1048577".to_owned()),
        ],
        Vec::new(),
    );
    server.route(ROOT, declared);
    server.route(API, Reply::CloseDelimited(vec![b' '; 1024 * 1024 + 1]));
    let (discovery, _) = discovery();
    let error = resolve(&discovery, &server.api_endpoint())
        .await
        .unwrap_err();
    let base = &server.base;
    assert_eq!(
        error.message(),
        format!(
            "No usable instance discovery document was served for {base}/api: \
             {base}{ROOT} (Instance response exceeds 1048576 bytes); \
             {base}{API} (Instance response exceeds 1048576 bytes)"
        )
    );
    assert_eq!(error.name(), DISCOVERY_UNREACHABLE_ERROR_NAME);
}

#[tokio::test]
async fn a_body_of_exactly_one_mebibyte_is_read() {
    let server = TestServer::start().await;
    let mut body = document(&server.base).to_string().into_bytes();
    body.resize(1024 * 1024, b' ');
    server.route(ROOT, Reply::CloseDelimited(body));
    let (discovery, _) = discovery();
    assert!(resolve(&discovery, &server.api_endpoint()).await.is_ok());
}

#[tokio::test]
async fn server_errors_fall_back_to_the_remembered_document() {
    let server = TestServer::start().await;
    server.route(ROOT, Reply::json(&document(&server.base)));
    let (discovery, markers) = discovery();
    let served = resolve(&discovery, &server.api_endpoint()).await.unwrap();
    markers
        .written(&format!("runtime_discovery.v1:{}", server.api_endpoint()))
        .await;

    server.route(ROOT, Reply::status(500));
    server.route(API, Reply::status(503));
    let remembered = resolve(&discovery, &server.api_endpoint()).await.unwrap();
    assert_eq!(remembered, served);
}

#[tokio::test]
async fn server_errors_without_a_remembered_document_are_unreachable() {
    let server = TestServer::start().await;
    server.route(ROOT, Reply::status(500));
    server.route(API, Reply::status(502));
    let (discovery, _) = discovery();
    let error = resolve(&discovery, &server.api_endpoint())
        .await
        .unwrap_err();
    assert_eq!(error.name(), DISCOVERY_UNREACHABLE_ERROR_NAME);
    assert!(error.message().contains("(responded HTTP 500)"));
    assert!(error.message().contains("(responded HTTP 502)"));
}

/// The fallback reads markers from the real desktop app store.
#[test]
fn the_fallback_reads_markers_from_the_app_store() {
    let directory = tempfile::tempdir().unwrap();
    let i18n = crate::i18n::MainI18n::load(directory.path());
    let storage = crate::storage::open_desktop_app_storage(directory.path(), &i18n);
    let markers = Markers::over(
        storage.clone(),
        |storage, key| async move { storage.get_marker(&key).await },
        |storage, key, value| async move { storage.set_marker(&key, &value).await },
    );
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let server = TestServer::start().await;
        server.route(ROOT, Reply::json(&document(&server.base)));
        let discovery = RuntimeDiscovery::new(outbound(), markers);
        let served = resolve(&discovery, &server.api_endpoint()).await.unwrap();
        let key = format!("runtime_discovery.v1:{}", server.api_endpoint());
        for _ in 0..200 {
            if storage.get_marker(&key).await.unwrap().is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        // A 404 answer from either candidate would mean the instance is up, so both fail.
        server.route(ROOT, Reply::status(500));
        server.route(API, Reply::status(500));
        assert_eq!(
            resolve(&discovery, &server.api_endpoint()).await.unwrap(),
            served
        );
    });
}

#[tokio::test]
async fn a_document_declared_elsewhere_is_followed_once() {
    let declared = TestServer::start().await;
    declared.route(ROOT, Reply::json(&document(&declared.base)));
    let entered = TestServer::start().await;
    // Served by `entered`, declaring `declared`'s API.
    entered.route(ROOT, Reply::json(&document(&declared.base)));
    let (discovery, markers) = discovery();

    let plan = resolve(&discovery, &entered.api_endpoint()).await.unwrap();
    assert_eq!(plan.instance_key, declared.api_endpoint());
    for endpoint in [declared.api_endpoint(), entered.api_endpoint()] {
        let marker = markers
            .written(&format!("runtime_discovery.v1:{endpoint}"))
            .await;
        assert_eq!(marker["url"], format!("{}{ROOT}", declared.base));
    }
}

#[tokio::test]
async fn a_second_declaration_elsewhere_is_not_followed() {
    let third = TestServer::start().await;
    third.route(ROOT, Reply::json(&document(&third.base)));
    let second = TestServer::start().await;
    second.route(ROOT, Reply::json(&document(&third.base)));
    let first = TestServer::start().await;
    first.route(ROOT, Reply::json(&document(&second.base)));
    let (discovery, _) = discovery();

    let error = resolve(&discovery, &first.api_endpoint())
        .await
        .unwrap_err();
    assert_eq!(error.name(), DISCOVERY_FAILED_ERROR_NAME);
    assert!(error.message().starts_with(&format!(
        "No usable instance discovery document was served for {}/api: ",
        second.base
    )));
    assert!(error.message().contains(&format!(
        "Instance discovery served by {} declared an API at {}/api",
        second.base, third.base
    )));
    assert!(third.requests().is_empty());
}

#[tokio::test]
async fn cancelled_discovery_is_not_unreachable() {
    let server = TestServer::start().await;
    server.route(ROOT, Reply::Hang);
    let (discovery, _) = discovery();
    let cancel = CancellationToken::new();
    let aborter = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        aborter.cancel();
    });
    let error = discovery
        .resolve(&server.api_endpoint(), &cancel)
        .await
        .unwrap_err();
    assert_eq!(error.name(), DISCOVERY_FAILED_ERROR_NAME);
    assert!(
        error
            .message()
            .contains("request was aborted by its caller")
    );
}

#[tokio::test]
async fn endpoint_input_must_be_an_absolute_http_endpoint() {
    let (discovery, _) = discovery();
    let error = resolve(&discovery, "/relative/api").await.unwrap_err();
    assert_eq!(error.name(), "DesktopRuntimeEndpointInputError");
    assert_eq!(
        error.message(),
        "Desktop runtime endpoint input is not an absolute http(s) API endpoint: /relative/api"
    );
    assert_eq!(
        require_api_endpoint("https://fluxer.com/api/").unwrap(),
        "https://web.fluxer.app/api"
    );
}
