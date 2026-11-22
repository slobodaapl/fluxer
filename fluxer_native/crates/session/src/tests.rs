// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use fluxer_protocol::Url;
use serde_json::json;

fn saved() -> SavedSession {
    let origin = Url::parse("https://chat.example/.well-known/fluxer").unwrap();
    let document = json!({"api_code_version":1,"endpoints":{
        "api":"https://chat.example/api","api_client":"https://chat.example/api","api_public":"https://chat.example/api",
        "gateway":"wss://chat.example/gateway","media":"https://chat.example/media","static_cdn":"https://chat.example/cdn",
        "marketing":"https://chat.example/marketing","admin":"https://chat.example/admin","invite":"https://chat.example/invite",
        "gift":"https://chat.example/gift","webapp":"https://chat.example"
    },"features":{"voice_enabled":true,"self_hosted":true}});
    SavedSession {
        instance: Instance::from_document(&origin, document).unwrap(),
        token: SessionToken::new("synthetic-session-secret").unwrap(),
        user_id: 100.into(),
        user: Some(json!({"id":"100","username":"synthetic","email":"private@example.test"})),
        locale: "en-US".into(),
        selected_guild: Some(1.into()),
        selected_channel: Some(11.into()),
        valid: true,
    }
}
fn encoded_value() -> Value {
    serde_json::from_slice(&codec::encode(&saved()).unwrap()).unwrap()
}
fn rejects(value: Value) {
    assert_eq!(
        codec::decode(&serde_json::to_vec(&value).unwrap()).unwrap_err(),
        StoreError::InvalidRecord
    );
}

#[test]
fn codec_roundtrip_preserves_instance_profile_selection_and_invalid_state() {
    let mut session = saved();
    session.valid = false;
    let bytes = codec::encode(&session).unwrap();
    let restored = codec::decode(&bytes).unwrap();
    assert_eq!(restored.account_key(), "https://chat.example/api::100");
    assert_eq!(
        restored.token.expose_secret(),
        session.token.expose_secret()
    );
    assert_eq!(restored.instance.document, session.instance.document);
    assert_eq!(restored.instance.features, session.instance.features);
    assert_eq!(restored.user, session.user);
    assert_eq!(restored.locale, "en-US");
    assert_eq!(restored.selected_guild, Some(1.into()));
    assert_eq!(restored.selected_channel, Some(11.into()));
    assert!(!restored.valid);
    let debug = format!("{restored:?}");
    assert!(!debug.contains("synthetic-session-secret"));
    assert!(!debug.contains("private@example.test"));
    assert!(!debug.contains("chat.example"));
}

#[test]
fn codec_rejects_foreign_endpoint_referrals_and_qualified_account_changes() {
    for field in ["api", "gateway", "web_app"] {
        let mut value = encoded_value();
        value["instance"][field] = json!(if field == "gateway" {
            "wss://other.example/gateway"
        } else {
            "https://other.example/api"
        });
        rejects(value);
    }
    let mut value = encoded_value();
    value["instance"]["document"]["endpoints"]["api_client"] =
        json!("https://chat.example/changed-api");
    rejects(value);
    let mut value = encoded_value();
    value["instance"]["document"]["endpoints"]["api"] =
        json!("https://declared-elsewhere.example/api");
    rejects(value);
    let mut value = encoded_value();
    value["account_key"] = json!("https://other.example/api::100");
    rejects(value);
    let mut value = encoded_value();
    value["user"]["id"] = json!("101");
    rejects(value);
    let mut value = encoded_value();
    value["user"] = Value::Null;
    value["user_id"] = json!("101");
    rejects(value);
    let mut value = encoded_value();
    value["instance"]["features"] = json!({"voice_enabled":false});
    rejects(value);
}

#[test]
fn codec_bounds_malformed_versions_credentials_and_selections_without_logging_secrets() {
    // AccountStorageKey.ts caps the instance-qualified key at 1024 UTF-8 bytes.
    let mut oversized = saved();
    oversized.instance = Instance::from_endpoints(
        &format!("https://chat.example/{}", "p".repeat(1024)),
        "wss://chat.example/gateway",
    )
    .unwrap();
    assert_eq!(
        codec::encode(&oversized).unwrap_err(),
        StoreError::InvalidRecord
    );
    for version in [json!(0), json!(2), json!("1")] {
        let mut value = encoded_value();
        value["version"] = version;
        rejects(value);
    }
    for version in [json!(0), json!(9_007_199_254_740_992_u64)] {
        let mut value = encoded_value();
        value["instance"]["api_version"] = version;
        rejects(value);
    }
    for token in [
        json!(""),
        json!("secret\nInjected: bad"),
        json!("x".repeat(4097)),
    ] {
        let mut value = encoded_value();
        value["token"] = token;
        rejects(value);
    }
    for key in ["user_id", "selected_guild", "selected_channel"] {
        let mut value = encoded_value();
        value[key] = json!("0");
        rejects(value);
    }
    let mut value = encoded_value();
    value["user_id"] = json!(100);
    rejects(value);
    let mut value = encoded_value();
    value["locale"] = json!("en-US\nInjected");
    rejects(value);
    let mut value = encoded_value();
    value["instance"]["api"] = json!("https://user:secret@chat.example/api");
    rejects(value);
    assert_eq!(
        codec::decode(b"{invalid synthetic-session-secret").unwrap_err(),
        StoreError::InvalidRecord
    );
    assert_eq!(
        codec::decode(&vec![b' '; codec::MAX_RECORD_BYTES + 1]).unwrap_err(),
        StoreError::InvalidRecord
    );
    for error in [
        StoreError::InvalidRecord,
        StoreError::Unavailable,
        StoreError::Unsupported,
    ] {
        assert!(!format!("{error:?} {error}").contains("synthetic-session-secret"));
    }
}

#[test]
fn explicit_local_endpoints_and_minimal_handoff_profile_are_pinned_without_discovery() {
    let mut session = saved();
    session.instance = Instance::from_endpoints(
        "http://127.0.0.1:1234/custom/api",
        "ws://127.0.0.1:1234/gateway",
    )
    .unwrap();
    session.user = Some(json!({"username":"synthetic","discriminator":"0001"}));
    let restored = codec::decode(&codec::encode(&session).unwrap()).unwrap();
    assert_eq!(
        restored.instance.api.as_str(),
        "http://127.0.0.1:1234/custom/api"
    );
    assert!(restored.instance.document.is_null());
    assert_eq!(
        restored.account_key(),
        "http://127.0.0.1:1234/custom/api::100"
    );
}

#[tokio::test]
async fn shared_memory_store_roundtrip_survives_worker_recreation_and_clear() {
    let store = MemorySessionStore::default();
    let next_worker = store.clone();
    let store: Arc<dyn SessionStore> = Arc::new(store);
    tokio::spawn(async move {
        store.save(saved()).await.unwrap();
    })
    .await
    .unwrap();
    let record = next_worker.load().await.unwrap().unwrap();
    assert_eq!(record.user_id, 100.into());
    let last_worker = next_worker.clone();
    next_worker.clear().await.unwrap();
    assert!(last_worker.load().await.unwrap().is_none());
    assert!(!format!("{last_worker:?}").contains("synthetic-session-secret"));
}

#[tokio::test]
async fn independent_memory_stores_and_replaced_accounts_do_not_mix() {
    let first = MemorySessionStore::default();
    let isolated = MemorySessionStore::default();
    first.save(saved()).await.unwrap();
    assert!(isolated.load().await.unwrap().is_none());
    let mut another = saved();
    another.user_id = 200.into();
    another.user = Some(json!({"id":"200","username":"other"}));
    another.token = SessionToken::new("other-synthetic-token").unwrap();
    first.save(another).await.unwrap();
    let loaded = first.load().await.unwrap().unwrap();
    assert_eq!(loaded.user_id, 200.into());
    assert_eq!(loaded.token.expose_secret(), "other-synthetic-token");
    let mut invalid = loaded.clone();
    invalid.valid = false;
    first.save(invalid).await.unwrap();
    assert!(!first.load().await.unwrap().unwrap().valid);
}

#[tokio::test]
async fn malformed_memory_record_is_reported_without_deleting_or_replacing_it() {
    let store = MemorySessionStore::default();
    *store.record.lock().unwrap() = Some(Zeroizing::new(b"malformed secret marker".to_vec()));
    assert_eq!(store.load().await.unwrap_err(), StoreError::InvalidRecord);
    assert!(store.record.lock().unwrap().is_some());
    store.clear().await.unwrap();
    assert!(store.load().await.unwrap().is_none());
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[tokio::test]
#[ignore = "opt-in synthetic OS credential-vault CRUD; set FLUXER_SESSION_VAULT_TEST=1"]
async fn synthetic_vault_roundtrip_is_scoped_and_cleans_up() {
    assert_eq!(
        std::env::var("FLUXER_SESSION_VAULT_TEST").as_deref(),
        Ok("1"),
        "explicit synthetic-vault test opt-in required"
    );
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let application = format!(
        "app.fluxer.Native.synthetic.{}.{unique}",
        std::process::id()
    );
    let primary = OsSessionStore::synthetic(application.clone());
    let isolated = OsSessionStore::synthetic(format!("{application}.isolated"));
    let result = async {
        if primary.load().await?.is_some() || isolated.load().await?.is_some() {
            return Err(StoreError::InvalidRecord);
        }
        primary.save(saved()).await?;
        isolated.save(saved()).await?;
        let reloaded = OsSessionStore::synthetic(application)
            .load()
            .await?
            .ok_or(StoreError::InvalidRecord)?;
        if reloaded.account_key() != saved().account_key()
            || reloaded.token.expose_secret() != "synthetic-session-secret"
        {
            return Err(StoreError::InvalidRecord);
        }
        let mut replaced = reloaded;
        replaced.selected_channel = None;
        primary.save(replaced).await?;
        if primary
            .load()
            .await?
            .ok_or(StoreError::InvalidRecord)?
            .selected_channel
            .is_some()
        {
            return Err(StoreError::InvalidRecord);
        }
        primary.clear().await?;
        if primary.load().await?.is_some() || isolated.load().await?.is_none() {
            return Err(StoreError::InvalidRecord);
        }
        Ok::<_, StoreError>(())
    }
    .await;
    let cleanup_primary = primary.clear().await;
    let cleanup_isolated = isolated.clear().await;
    assert_eq!(result, Ok(()));
    assert_eq!(cleanup_primary, Ok(()));
    assert_eq!(cleanup_isolated, Ok(()));
}
