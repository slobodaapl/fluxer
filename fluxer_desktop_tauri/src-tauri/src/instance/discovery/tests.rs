// SPDX-License-Identifier: AGPL-3.0-or-later

use serde_json::{Value, json};

use super::*;

/// `DEPLOYED_OFFICIAL_DOCUMENT` from `packages/instance_bootstrap/src/__tests__/DiscoveryFixtures.ts`.
fn deployed_official_document() -> Value {
    json!({
        "api_code_version": 1,
        "endpoints": {
            "api": "https://api.fluxer.app",
            "api_client": "https://api.fluxer.app",
            "api_public": "https://api.fluxer.app",
            "gateway": "wss://gateway.fluxer.app",
            "media": "https://media.fluxer.app",
            "static_cdn": "https://cdn.fluxer.app",
            "marketing": "https://fluxer.app",
            "admin": "https://admin.fluxer.app",
            "invite": "https://fluxer.gg",
            "gift": "https://fluxer.gift",
            "webapp": "https://web.fluxer.app"
        },
        "captcha": {"provider": "altcha"},
        "features": {
            "voice_enabled": true,
            "stripe_enabled": true,
            "self_hosted": false,
            "presigned_attachment_uploads": true,
            "emails_enabled": true
        },
        "gif": {"provider": "klipy", "display_name": "Klipy", "attribution_required": false},
        "sso": {
            "enabled": false,
            "enforced": false,
            "display_name": null,
            "button_label": null,
            "redirect_uri": "https://web.fluxer.app/sso/callback"
        },
        "registration": {"mode": "open", "admin_registration_urls_enabled": true},
        "community": {"single_community": false, "single_community_guild_id": null, "direct_messages_disabled": false},
        "services": {"gif_enabled": true, "youtube_enabled": true, "bluesky_enabled": false},
        "limits": {
            "version": 2,
            "traitDefinitions": ["premium"],
            "rules": [
                {"id": "default", "overrides": {"max_guild_emojis": 50}},
                {"id": "premium", "filters": {"traits": ["premium"], "guildFeatures": []}, "overrides": {"max_guild_emojis": 250}}
            ],
            "defaultsHash": "deployed-defaults-hash"
        },
        "push": {"public_vapid_key": "BPublicVapidKey"},
        "app_public": {
            "branding": {
                "product_name": "Fluxer",
                "icon_url": "https://cdn.fluxer.app/icon.png",
                "symbol_url": "https://cdn.fluxer.app/symbol.png",
                "logo_url": "https://cdn.fluxer.app/logo.png",
                "wordmark_url": "https://cdn.fluxer.app/wordmark.png",
                "favicon_url": "https://cdn.fluxer.app/favicon.png",
                "theme_color": "#5865f2"
            },
            "setup": {"configured": true, "admin_url": "https://admin.fluxer.app"},
            "legal": {"terms_url": "https://fluxer.app/terms", "privacy_url": "https://fluxer.app/privacy"},
            "registration": {"collect_date_of_birth": true}
        }
    })
}

/// `SELF_HOSTED_API_TOPOLOGY_DOCUMENT` from the same fixtures file.
fn self_hosted_document() -> Value {
    json!({
        "api_code_version": 1,
        "endpoints": {
            "api": "https://chat.example.test/api",
            "api_client": "https://chat.example.test/api",
            "api_public": "https://chat.example.test/api",
            "gateway": "wss://chat.example.test/gateway",
            "media": "https://chat.example.test/media",
            "static_cdn": "https://chat.example.test",
            "marketing": "https://chat.example.test/marketing",
            "admin": "https://chat.example.test/admin",
            "invite": "https://chat.example.test/invite",
            "gift": "https://chat.example.test/gift",
            "webapp": "https://chat.example.test"
        },
        "captcha": {"provider": "none"},
        "features": {
            "voice_enabled": false,
            "stripe_enabled": false,
            "self_hosted": true,
            "presigned_attachment_uploads": false,
            "emails_enabled": false
        },
        "gif": {"provider": "klipy", "display_name": "Klipy", "attribution_required": false},
        "sso": {
            "enabled": false,
            "enforced": false,
            "display_name": null,
            "button_label": null,
            "redirect_uri": "https://chat.example.test/sso/callback"
        },
        "registration": {"mode": "closed", "admin_registration_urls_enabled": false},
        "community": {"single_community": true, "single_community_guild_id": "1", "direct_messages_disabled": false},
        "services": {"gif_enabled": false, "youtube_enabled": false, "bluesky_enabled": false},
        "limits": {"version": 1, "traitDefinitions": [], "rules": []},
        "push": {"public_vapid_key": null},
        "app_public": {
            "branding": {
                "product_name": "Example Chat",
                "icon_url": null,
                "symbol_url": null,
                "logo_url": null,
                "wordmark_url": null,
                "favicon_url": null,
                "theme_color": null
            },
            "setup": {"configured": true, "admin_url": "https://chat.example.test/admin"},
            "legal": {"terms_url": null, "privacy_url": null},
            "registration": {"collect_date_of_birth": false}
        }
    })
}

fn parse(document: &Value) -> InstanceDiscovery {
    parse_instance_discovery_document(document).unwrap()
}

fn invalid_message(error: DiscoveryError) -> String {
    error.to_string()
}

/// Removes the value at a dotted path.
fn without(mut document: Value, path: &str) -> Value {
    let (parents, leaf) = path.rsplit_once('.').unwrap_or(("", path));
    let mut node = &mut document;
    for key in parents.split('.').filter(|key| !key.is_empty()) {
        node = node.get_mut(key).unwrap();
    }
    node.as_object_mut().unwrap().remove(leaf).unwrap();
    document
}

fn with(mut document: Value, path: &str, value: Value) -> Value {
    let (parents, leaf) = path.rsplit_once('.').unwrap_or(("", path));
    let mut node = &mut document;
    for key in parents.split('.').filter(|key| !key.is_empty()) {
        node = node.get_mut(key).unwrap();
    }
    node.as_object_mut().unwrap().insert(leaf.to_owned(), value);
    document
}

#[test]
fn candidate_urls_try_the_root_then_the_carried_path_then_the_conventional_api_path() {
    for (input, expected) in [
        (
            "fluxer.app",
            vec![
                "https://fluxer.app/.well-known/fluxer",
                "https://fluxer.app/api/.well-known/fluxer",
            ],
        ),
        (
            "https://chat.example.test/custom/",
            vec![
                "https://chat.example.test/.well-known/fluxer",
                "https://chat.example.test/custom/.well-known/fluxer",
                "https://chat.example.test/api/.well-known/fluxer",
            ],
        ),
        (
            "https://chat.example.test:8443/api",
            vec![
                "https://chat.example.test:8443/.well-known/fluxer",
                "https://chat.example.test:8443/api/.well-known/fluxer",
            ],
        ),
        (
            "http://localhost:3000",
            vec![
                "http://localhost:3000/.well-known/fluxer",
                "http://localhost:3000/api/.well-known/fluxer",
            ],
        ),
        (
            "https://[::1]:443/x",
            vec![
                "https://[::1]/.well-known/fluxer",
                "https://[::1]/x/.well-known/fluxer",
                "https://[::1]/api/.well-known/fluxer",
            ],
        ),
    ] {
        assert_eq!(discovery_candidate_urls(input), expected, "{input}");
    }
}

#[test]
fn candidate_urls_are_empty_for_input_that_is_not_an_http_endpoint() {
    for input in [
        "",
        "   ",
        "ftp://example.test",
        "wss://example.test",
        "/api",
        "https://u@example.test",
    ] {
        assert!(discovery_candidate_urls(input).is_empty(), "{input}");
    }
}

#[test]
fn the_deployed_official_document_parses_and_projects() {
    let document = deployed_official_document();
    let parsed = parse(&document);
    assert_eq!(parsed.raw, document);
    assert_eq!(parsed.generation, InstanceGeneration::Legacy);
    assert_eq!(parsed.api_code_version, Some(1));
    assert_eq!(parsed.endpoints.api, "https://api.fluxer.app");
    assert_eq!(parsed.endpoints.gateway, "wss://gateway.fluxer.app");
    assert_eq!(parsed.endpoints.upload_relay, None);
    assert_eq!(
        parsed.sso.as_ref().unwrap().redirect_uri,
        "https://web.fluxer.app/sso/callback"
    );
    assert_eq!(
        parsed.limits,
        Some(LimitConfig::Wire {
            trait_definitions: vec!["premium".to_owned()],
            rules: vec![
                LimitRule {
                    id: "default".to_owned(),
                    filters: None,
                    values: vec![("max_guild_emojis".to_owned(), 50.0)],
                },
                LimitRule {
                    id: "premium".to_owned(),
                    filters: Some(LimitFilter {
                        traits: Some(vec!["premium".to_owned()]),
                        guild_features: Some(vec![]),
                    }),
                    values: vec![("max_guild_emojis".to_owned(), 250.0)],
                },
            ],
            defaults_hash: "deployed-defaults-hash".to_owned(),
        })
    );
    require_discovery_declares_its_own_origin("https://api.fluxer.app/.well-known/fluxer", &parsed)
        .unwrap();

    let runtime = project_client_runtime_discovery(&parsed).unwrap();
    assert_eq!(runtime.generation, InstanceGeneration::Legacy);
    assert_eq!(runtime.api_code_version, 1);
    assert_eq!(runtime.endpoints.discovery_api, "https://api.fluxer.app");
    assert_eq!(runtime.endpoints.api_client, "https://api.fluxer.app");
    assert_eq!(runtime.endpoints.api_public, "https://api.fluxer.app");
    assert_eq!(runtime.endpoints.gateway, "wss://gateway.fluxer.app");
    assert_eq!(runtime.endpoints.media, "https://media.fluxer.app");
    assert_eq!(runtime.endpoints.upload_relay, None);
    assert_eq!(runtime.endpoints.static_cdn, "https://cdn.fluxer.app");
    assert_eq!(runtime.endpoints.marketing, "https://fluxer.app");
    assert_eq!(runtime.endpoints.docs, None);
    assert_eq!(runtime.endpoints.admin, "https://admin.fluxer.app");
    assert_eq!(runtime.endpoints.invite, "https://fluxer.gg");
    assert_eq!(runtime.endpoints.gift, "https://fluxer.gift");
    assert_eq!(runtime.endpoints.webapp, "https://web.fluxer.app");
    assert!(runtime.features.voice_enabled);
    assert!(!runtime.features.self_hosted);
    assert!(runtime.features.premium_enabled, "defaults to !self_hosted");
    assert!(!runtime.features.stripe_serviceable);
    assert!(!runtime.features.phone_verification_enabled);
    assert_eq!(
        runtime.push_public_vapid_key.as_deref(),
        Some("BPublicVapidKey")
    );
    assert_eq!(runtime.premium_product_name, "Plutonium");
}

#[test]
fn the_self_hosted_document_projects_with_null_branding_and_push_key() {
    let parsed = parse(&self_hosted_document());
    assert_eq!(
        parsed.limits,
        Some(LimitConfig::Snapshot {
            version: Some(1),
            trait_definitions: vec![],
            rules: vec![],
        })
    );
    assert_eq!(
        parsed.app_public.as_ref().unwrap().branding.icon_url,
        Some(None)
    );
    require_discovery_declares_its_own_origin(
        "https://chat.example.test/api/.well-known/fluxer",
        &parsed,
    )
    .unwrap();

    let runtime = project_client_runtime_discovery(&parsed).unwrap();
    assert!(runtime.features.self_hosted);
    assert!(!runtime.features.premium_enabled);
    assert_eq!(runtime.push_public_vapid_key, None);
    assert_eq!(
        runtime.endpoints.api_client,
        "https://chat.example.test/api"
    );
    assert_eq!(runtime.endpoints.static_cdn, "https://chat.example.test");
}

#[test]
fn an_explicit_premium_flag_and_product_name_win_over_the_defaults() {
    let mut document = with(
        self_hosted_document(),
        "features.premium_enabled",
        json!(true),
    );
    document = with(document, "features.stripe_serviceable", json!(true));
    document = with(
        document,
        "app_public.branding.premium_product_name",
        json!("Gold"),
    );
    let runtime = project_client_runtime_discovery(&parse(&document)).unwrap();
    assert!(runtime.features.premium_enabled);
    assert!(runtime.features.stripe_serviceable);
    assert_eq!(runtime.premium_product_name, "Gold");
}

#[test]
fn a_non_object_body_is_refused() {
    for body in [json!(null), json!([]), json!("x"), json!(1), json!(true)] {
        let message = invalid_message(parse_instance_discovery_document(&body).unwrap_err());
        assert_eq!(
            message,
            "Instance discovery document is unusable: the response body is not a JSON object"
        );
    }
}

#[test]
fn the_current_codename_is_the_tungsten_generation() {
    let parsed = parse(&with(
        deployed_official_document(),
        "codename",
        json!("tungsten"),
    ));
    assert_eq!(parsed.generation, InstanceGeneration::Tungsten);
}

#[test]
fn another_codename_requires_a_newer_client_before_anything_else_is_validated() {
    let error = parse_instance_discovery_document(&json!({"codename": "osmium"})).unwrap_err();
    assert_eq!(
        error,
        DiscoveryError::RequiresNewerClient("osmium".to_owned())
    );
    assert_eq!(
        error.to_string(),
        "Instance speaks generation \"osmium\", which this client does not understand"
    );
}

#[test]
fn an_empty_or_non_string_codename_is_ignored() {
    for codename in [json!(""), json!(7), json!(null)] {
        let parsed = parse(&with(deployed_official_document(), "codename", codename));
        assert_eq!(parsed.generation, InstanceGeneration::Legacy);
    }
}

#[test]
fn api_code_version_must_be_a_positive_safe_integer_when_present() {
    for bad in [
        json!(0),
        json!(-1),
        json!(1.5),
        json!("1"),
        json!(true),
        json!(9_007_199_254_740_992_u64),
        json!(u64::MAX),
    ] {
        let message = invalid_message(
            parse_instance_discovery_document(&with(
                deployed_official_document(),
                "api_code_version",
                bad.clone(),
            ))
            .unwrap_err(),
        );
        assert_eq!(
            message,
            "Instance discovery document is unusable: api_code_version is not a positive integer",
            "{bad}"
        );
    }
    for good in [
        json!(1.0),
        json!(2),
        json!(9_007_199_254_740_991_u64),
        json!(1e3),
    ] {
        let parsed = parse(&with(
            deployed_official_document(),
            "api_code_version",
            good.clone(),
        ));
        assert!(parsed.api_code_version.is_some(), "{good}");
    }
    let parsed = parse(&with(
        deployed_official_document(),
        "api_code_version",
        json!(1.0),
    ));
    assert_eq!(parsed.api_code_version, Some(1));
}

#[test]
fn a_missing_or_null_api_code_version_parses_but_fails_projection() {
    for document in [
        without(deployed_official_document(), "api_code_version"),
        with(
            deployed_official_document(),
            "api_code_version",
            json!(null),
        ),
    ] {
        let parsed = parse(&document);
        assert_eq!(parsed.api_code_version, None);
        assert_eq!(
            invalid_message(project_client_runtime_discovery(&parsed).unwrap_err()),
            "Instance discovery document is unusable: api_code_version is required by the client runtime"
        );
    }
}

#[test]
fn endpoints_api_and_gateway_are_required_to_parse() {
    for (document, reason) in [
        (
            without(deployed_official_document(), "endpoints"),
            "endpoints is missing or not an object",
        ),
        (
            with(deployed_official_document(), "endpoints", json!([])),
            "endpoints is missing or not an object",
        ),
        (
            without(deployed_official_document(), "endpoints.api"),
            "endpoints.api is missing or not an absolute http(s) endpoint",
        ),
        (
            with(
                deployed_official_document(),
                "endpoints.api",
                json!("wss://api.fluxer.app"),
            ),
            "endpoints.api is missing or not an absolute http(s) endpoint",
        ),
        (
            without(deployed_official_document(), "endpoints.gateway"),
            "endpoints.gateway is missing or not a ws(s) endpoint",
        ),
        (
            with(
                deployed_official_document(),
                "endpoints.gateway",
                json!("https://gateway.fluxer.app"),
            ),
            "endpoints.gateway is missing or not a ws(s) endpoint",
        ),
    ] {
        let message = invalid_message(parse_instance_discovery_document(&document).unwrap_err());
        assert_eq!(
            message,
            format!("Instance discovery document is unusable: {reason}")
        );
    }
}

#[test]
fn endpoints_are_canonicalised_while_parsing() {
    let mut document = with(
        deployed_official_document(),
        "endpoints.api",
        json!(" API.fluxer.app:443/ "),
    );
    document = with(
        document,
        "endpoints.gateway",
        json!("wss://gateway.fluxer.app:443/"),
    );
    document = with(
        document,
        "endpoints.webapp",
        json!("https://web.fluxer.app/not-an-origin"),
    );
    let parsed = parse(&document);
    assert_eq!(parsed.endpoints.api, "https://api.fluxer.app");
    assert_eq!(parsed.endpoints.gateway, "wss://gateway.fluxer.app");
    assert_eq!(parsed.endpoints.webapp, None);
    assert_eq!(parsed.raw, document, "the raw document is kept unchanged");
}

#[test]
fn each_projection_requirement_is_enforced_by_name() {
    for path in [
        "endpoints.api_client",
        "endpoints.api_public",
        "endpoints.media",
        "endpoints.static_cdn",
        "endpoints.marketing",
        "endpoints.admin",
        "endpoints.invite",
        "endpoints.gift",
        "endpoints.webapp",
        "features.voice_enabled",
        "features.stripe_enabled",
        "features.self_hosted",
        "features.presigned_attachment_uploads",
        "features.emails_enabled",
        "gif",
        "registration",
        "community",
        "services",
        "limits",
        "push.public_vapid_key",
        "app_public",
        "app_public.branding.product_name",
        "app_public.branding.icon_url",
        "app_public.branding.symbol_url",
        "app_public.branding.logo_url",
        "app_public.branding.wordmark_url",
        "app_public.branding.favicon_url",
        "app_public.branding.theme_color",
        "app_public.setup.configured",
        "app_public.setup.admin_url",
        "app_public.legal.terms_url",
        "app_public.legal.privacy_url",
        "app_public.registration.collect_date_of_birth",
    ] {
        let parsed = parse(&without(deployed_official_document(), path));
        let message = invalid_message(project_client_runtime_discovery(&parsed).unwrap_err());
        assert_eq!(
            message,
            format!(
                "Instance discovery document is unusable: {path} is required by the client runtime"
            ),
        );
    }
}

#[test]
fn an_unusable_required_section_is_reported_the_same_as_a_missing_one() {
    for (path, bad) in [
        ("endpoints.media", json!("ftp://media.fluxer.app")),
        ("features.voice_enabled", json!("yes")),
        ("gif", json!({"provider": "klipy"})),
        (
            "registration",
            json!({"mode": "invite-only", "admin_registration_urls_enabled": true}),
        ),
        ("community", json!({"single_community": false})),
        ("services", json!({"gif_enabled": true})),
        ("limits", json!({"traitDefinitions": [], "rules": "x"})),
        ("app_public.branding.product_name", json!("")),
        ("app_public.branding.icon_url", json!(7)),
    ] {
        let parsed = parse(&with(deployed_official_document(), path, bad.clone()));
        let result = project_client_runtime_discovery(&parsed);
        if path == "app_public.branding.icon_url" {
            // Present-but-unusable URLs project as null, not as missing.
            assert!(result.is_ok(), "{path}");
            continue;
        }
        assert_eq!(
            invalid_message(result.unwrap_err()),
            format!(
                "Instance discovery document is unusable: {path} is required by the client runtime"
            ),
            "{bad}"
        );
    }
}

#[test]
fn projection_reports_the_first_missing_requirement_in_typescript_order() {
    let mut document = without(deployed_official_document(), "gif");
    document = without(document, "endpoints.gift");
    document = without(document, "features.emails_enabled");
    document = without(document, "app_public.branding.icon_url");
    let order = [
        "endpoints.gift",
        "features.emails_enabled",
        "gif",
        "app_public.branding.icon_url",
    ];
    for expected in order {
        let message =
            invalid_message(project_client_runtime_discovery(&parse(&document)).unwrap_err());
        assert!(
            message.contains(expected),
            "{message} should name {expected}"
        );
        document = match expected {
            "endpoints.gift" => with(document, "endpoints.gift", json!("https://fluxer.gift")),
            "features.emails_enabled" => with(document, "features.emails_enabled", json!(true)),
            "gif" => with(
                document,
                "gif",
                json!({"provider": "k", "display_name": "K", "attribution_required": true}),
            ),
            _ => with(
                document,
                "app_public.branding.icon_url",
                json!("https://cdn.fluxer.app/i.png"),
            ),
        };
    }
    project_client_runtime_discovery(&parse(&document)).unwrap();
}

#[test]
fn the_origin_check_names_the_declared_endpoint_that_disagrees() {
    let parsed = parse(&with(
        deployed_official_document(),
        "endpoints.api_client",
        json!("https://elsewhere.example.test/api"),
    ));
    let error = require_discovery_declares_its_own_origin(
        "https://api.fluxer.app/.well-known/fluxer",
        &parsed,
    )
    .unwrap_err();
    assert_eq!(
        error,
        DiscoveryError::OriginMismatch {
            served_by: "https://api.fluxer.app".to_owned(),
            declared: "https://elsewhere.example.test/api".to_owned(),
        }
    );
    assert_eq!(
        error.to_string(),
        "Instance discovery served by https://api.fluxer.app declared an API at https://elsewhere.example.test/api"
    );
}

#[test]
fn the_origin_check_compares_scheme_host_and_port() {
    let parsed = parse(&deployed_official_document());
    for served in [
        "http://api.fluxer.app/.well-known/fluxer",
        "https://api.fluxer.app:8443/.well-known/fluxer",
        "https://fluxer.app/.well-known/fluxer",
    ] {
        assert!(
            matches!(
                require_discovery_declares_its_own_origin(served, &parsed),
                Err(DiscoveryError::OriginMismatch { .. })
            ),
            "{served}"
        );
    }
    require_discovery_declares_its_own_origin("https://API.fluxer.app:443/x?y#z", &parsed).unwrap();
}

#[test]
fn an_unparseable_served_url_is_reported_verbatim_with_the_declared_api() {
    let parsed = parse(&deployed_official_document());
    assert_eq!(
        require_discovery_declares_its_own_origin("not a url", &parsed).unwrap_err(),
        DiscoveryError::OriginMismatch {
            served_by: "not a url".to_owned(),
            declared: "https://api.fluxer.app".to_owned(),
        }
    );
}

#[test]
fn features_keep_only_known_flags_and_enum_values() {
    let document = with(
        deployed_official_document(),
        "features",
        json!({
            "voice_enabled": "true",
            "self_hosted": true,
            "identifier_first": false,
            "account_identity": "username",
            "tag_style": "weird",
            "instant_account_availability": "captcha_off",
            "interaction_tier": "enforced",
            "guild_join_gate": "visits_capable",
            "email_delivery_state": "off_incomplete",
            "verification_basis": "auto",
            "account_deletion_grace_period_hours": 8761
        }),
    );
    let features = parse(&document).features;
    assert_eq!(features.voice_enabled, None);
    assert_eq!(features.self_hosted, Some(true));
    assert_eq!(features.identifier_first, Some(false));
    assert_eq!(features.account_identity.as_deref(), Some("username"));
    assert_eq!(features.tag_style, None);
    assert_eq!(
        features.instant_account_availability.as_deref(),
        Some("captcha_off")
    );
    assert_eq!(features.interaction_tier.as_deref(), Some("enforced"));
    assert_eq!(features.guild_join_gate.as_deref(), Some("visits_capable"));
    assert_eq!(
        features.email_delivery_state.as_deref(),
        Some("off_incomplete")
    );
    assert_eq!(
        features.verification_basis, None,
        "`auto` is not a published basis"
    );
    assert_eq!(features.account_deletion_grace_period_hours, None);
    let bounded = with(
        deployed_official_document(),
        "features",
        json!({"account_deletion_grace_period_hours": 8760}),
    );
    assert_eq!(
        parse(&bounded).features.account_deletion_grace_period_hours,
        Some(8760)
    );
}

#[test]
fn sub_parsers_never_throw_and_fall_back_to_null_or_empty() {
    let mut document = deployed_official_document();
    for key in [
        "features",
        "gif",
        "sso",
        "registration",
        "community",
        "services",
        "limits",
        "push",
        "app_public",
        "age_policy",
        "domain_migration",
        "notice",
    ] {
        document = with(document, key, json!("garbage"));
    }
    let parsed = parse(&document);
    assert_eq!(parsed.features, InstanceFeatures::default());
    assert_eq!(parsed.push, InstancePush::default());
    assert!(parsed.gif.is_none() && parsed.sso.is_none() && parsed.registration.is_none());
    assert!(parsed.community.is_none() && parsed.services.is_none() && parsed.limits.is_none());
    assert!(parsed.app_public.is_none() && parsed.age_policy.is_none());
    assert!(parsed.domain_migration.is_none() && parsed.notice.is_none());
}

#[test]
fn sso_needs_its_flags_and_an_http_redirect() {
    let sso = |value: Value| parse(&with(deployed_official_document(), "sso", value)).sso;
    let base = json!({"enabled": true, "enforced": false, "redirect_uri": " https://idp.example.test/cb ", "display_name": "IdP", "button_label": "", "available": true});
    let parsed = sso(base.clone()).unwrap();
    assert_eq!(
        parsed.redirect_uri, "https://idp.example.test/cb",
        "trimmed but not otherwise rewritten"
    );
    assert_eq!(parsed.display_name.as_deref(), Some("IdP"));
    assert_eq!(parsed.button_label, None);
    assert_eq!(parsed.available, Some(true));
    assert!(
        sso(with(
            base.clone(),
            "redirect_uri",
            json!("ftp://idp.example.test")
        ))
        .is_none()
    );
    assert!(sso(with(base.clone(), "redirect_uri", json!("/relative"))).is_none());
    assert!(sso(without(base, "enforced")).is_none());
}

#[test]
fn community_defaults_follow_direct_messages_and_open_guild_creation() {
    let community = |value: Value| {
        parse(&with(deployed_official_document(), "community", value))
            .community
            .unwrap()
    };
    let parsed = community(json!({"single_community": false, "direct_messages_disabled": true}));
    assert!(parsed.friends_disabled);
    assert!(parsed.guild_create_access);
    let parsed = community(
        json!({"single_community": true, "single_community_guild_id": "", "direct_messages_disabled": true, "friends_disabled": false, "guild_create_access": false}),
    );
    assert!(!parsed.friends_disabled);
    assert!(!parsed.guild_create_access);
    assert_eq!(parsed.single_community_guild_id, None);
}

#[test]
fn notice_needs_an_id_and_message_and_keeps_only_parsable_dates() {
    let notice = |value: Value| parse(&with(deployed_official_document(), "notice", value)).notice;
    let parsed = notice(json!({"id": "n1", "message": "Maintenance", "url": "", "starts_at": "2030-01-01T00:00:00Z", "ends_at": "soon"})).unwrap();
    assert_eq!(parsed.url, None);
    assert_eq!(parsed.starts_at.as_deref(), Some("2030-01-01T00:00:00Z"));
    assert_eq!(parsed.ends_at, None);
    assert!(notice(json!({"id": "n1"})).is_none());
    assert!(notice(json!({"id": "", "message": "m"})).is_none());
}

#[test]
fn date_strings_follow_date_parse_for_the_iso_format() {
    for accepted in [
        "2021",
        "2021-06",
        "2021-06-15",
        "2021-02-31",
        "2021-01-01T10:00",
        "2021-01-01T10:00Z",
        "2021-01-01t10:00z",
        "2021-01-01 10:00",
        "2021-01-01T24:00:00Z",
        "2021-01-01T10:00:00.1234Z",
        "2021-01-01T10:00:00+0100",
        "2021-01-01T10:00:00-05:30",
        "+002021-01-01",
        "  2021-01-01  ",
    ] {
        assert!(is_parsable_date(accepted), "{accepted}");
    }
    for rejected in [
        "",
        "soon",
        "2021-02-32",
        "2021-13-01",
        "2021-01-01T24:00:01Z",
        "2021-01-01T10:60Z",
        "2021-01-01T10",
        "2021-01-01T10:00:00.Z",
        "2021-01-01T10:00:00+25:00",
        "2021-01-01T10:00:00Zjunk",
    ] {
        assert!(!is_parsable_date(rejected), "{rejected}");
    }
}

#[test]
fn limits_parse_as_wire_or_snapshot_exactly_like_the_typescript_reader() {
    let limits = |value: Value| parse(&with(deployed_official_document(), "limits", value)).limits;

    let snapshot =
        limits(json!({"traitDefinitions": ["t"], "rules": [{"id": "a", "limits": {"x": 1.5}}]}))
            .unwrap();
    assert_eq!(
        snapshot,
        LimitConfig::Snapshot {
            version: None,
            trait_definitions: vec!["t".to_owned()],
            rules: vec![LimitRule {
                id: "a".to_owned(),
                filters: None,
                values: vec![("x".to_owned(), 1.5)]
            }],
        }
    );
    assert!(matches!(
        limits(json!({"version": 2, "traitDefinitions": [], "rules": [], "defaultsHash": "h"})),
        Some(LimitConfig::Wire { .. })
    ));
    assert!(
        limits(json!({"version": 2, "traitDefinitions": [], "rules": []})).is_none(),
        "wire config needs a hash"
    );
    assert!(matches!(
        limits(json!({"version": 3, "traitDefinitions": [], "rules": []})),
        Some(LimitConfig::Snapshot {
            version: Some(3),
            ..
        })
    ));
    assert!(limits(json!({"traitDefinitions": [], "rules": [{"id": "a", "overrides": {}}, {"id": "b", "limits": {}}], "defaultsHash": "h"})).is_none(), "mixed formats");
    assert!(limits(json!({"traitDefinitions": [], "rules": [{"id": "a"}]})).is_none());
    assert!(
        limits(json!({"traitDefinitions": [], "rules": [{"id": "a", "limits": {"x": "1"}}]}))
            .is_none()
    );
    assert!(
        limits(
            json!({"traitDefinitions": [], "rules": [{"id": "a", "limits": {}, "filters": null}]})
        )
        .is_none()
    );
    assert!(limits(json!({"traitDefinitions": [], "rules": [{"id": "", "limits": {}}]})).is_none());
    assert!(limits(json!({"traitDefinitions": [1], "rules": []})).is_none());
    let overrides_win = limits(json!({"traitDefinitions": [], "rules": [{"id": "a", "overrides": {"x": 1}, "limits": {"y": 2}}], "defaultsHash": "h"})).unwrap();
    assert!(
        matches!(overrides_win, LimitConfig::Wire { ref rules, .. } if rules[0].values == vec![("x".to_owned(), 1.0)])
    );
    let too_many: Vec<Value> = (0..101)
        .map(|i| json!({"id": format!("r{i}"), "limits": {}}))
        .collect();
    assert!(limits(json!({"traitDefinitions": [], "rules": too_many})).is_none());
    let at_cap: Vec<Value> = (0..100)
        .map(|i| json!({"id": format!("r{i}"), "limits": {}}))
        .collect();
    assert!(limits(json!({"traitDefinitions": [], "rules": at_cap})).is_some());
    let filter = limits(json!({"traitDefinitions": [], "rules": [{"id": "a", "limits": {}, "filters": {"traits": ["p"], "guildFeatures": "bad"}}]})).unwrap();
    assert!(matches!(filter, LimitConfig::Snapshot { ref rules, .. }
        if rules[0].filters == Some(LimitFilter { traits: Some(vec!["p".to_owned()]), guild_features: None })));
}

#[test]
fn push_distinguishes_an_absent_key_from_a_null_or_unusable_one() {
    let push = |value: Value| parse(&with(deployed_official_document(), "push", value)).push;
    let parsed = push(
        json!({"public_vapid_key": "k", "delivery_mode": "hybrid", "event_resolution_path": "/p", "external_relay_url": "https://relay.example.test", "relay_signing_key_hashes": ["a"]}),
    );
    assert_eq!(parsed.public_vapid_key, Some(Some("k".to_owned())));
    assert_eq!(parsed.delivery_mode.as_deref(), Some("hybrid"));
    assert_eq!(parsed.event_resolution_path.as_deref(), Some("/p"));
    assert_eq!(
        parsed.external_relay_url,
        Some(Some("https://relay.example.test".to_owned()))
    );
    assert_eq!(
        parsed.relay_signing_key_hashes,
        Some(Some(vec!["a".to_owned()]))
    );
    let parsed = push(
        json!({"public_vapid_key": "", "external_relay_url": "ftp://x", "relay_signing_key_hashes": [1], "delivery_mode": "carrier-pigeon"}),
    );
    assert_eq!(parsed.public_vapid_key, Some(None));
    assert_eq!(parsed.external_relay_url, Some(None));
    assert_eq!(parsed.relay_signing_key_hashes, Some(None));
    assert_eq!(parsed.delivery_mode, None);
    assert_eq!(push(json!({})).public_vapid_key, None);
}

#[test]
fn age_policy_requires_every_geo_to_be_valid() {
    let policy =
        |value: Value| parse(&with(deployed_official_document(), "age_policy", value)).age_policy;
    let parsed = policy(json!({"geos": [{"country_code": "GB", "action": "block"}, {"country_code": "US", "region_code": "TX", "action": "restrict"}]})).unwrap();
    assert_eq!(
        parsed.geos,
        vec![
            InstanceAgePolicyGeo {
                country_code: "GB".to_owned(),
                region_code: None,
                action: "block".to_owned()
            },
            InstanceAgePolicyGeo {
                country_code: "US".to_owned(),
                region_code: Some("TX".to_owned()),
                action: "restrict".to_owned()
            },
        ]
    );
    assert!(policy(json!({"geos": [{"country_code": "GB", "action": "ban"}]})).is_none());
    assert!(policy(json!({"geos": [{"country_code": "", "action": "block"}]})).is_none());
    assert!(policy(json!({"geos": "GB"})).is_none());
    let too_many: Vec<Value> = (0..513)
        .map(|_| json!({"country_code": "GB", "action": "block"}))
        .collect();
    assert!(policy(json!({"geos": too_many})).is_none());
}

#[test]
fn domain_migration_enforces_its_bounds_and_salt_alphabet() {
    let migration = |value: Value| {
        parse(&with(
            deployed_official_document(),
            "domain_migration",
            value,
        ))
        .domain_migration
    };
    let base = json!({"enabled": true, "anonymous_rollout_basis_points": 2500, "rollout_salt": "  salt 1  ", "standalone_forwarding": false});
    let parsed = migration(base.clone()).unwrap();
    assert_eq!(parsed.rollout_salt, "salt 1");
    assert_eq!(parsed.anonymous_rollout_basis_points, 2500);
    assert!(
        migration(with(
            base.clone(),
            "anonymous_rollout_basis_points",
            json!(10_000)
        ))
        .is_some()
    );
    assert!(
        migration(with(
            base.clone(),
            "anonymous_rollout_basis_points",
            json!(10_001)
        ))
        .is_none()
    );
    assert!(
        migration(with(
            base.clone(),
            "anonymous_rollout_basis_points",
            json!(-1)
        ))
        .is_none()
    );
    assert!(
        migration(with(
            base.clone(),
            "anonymous_rollout_basis_points",
            json!(0.5)
        ))
        .is_none()
    );
    assert!(migration(with(base.clone(), "rollout_salt", json!("é"))).is_none());
    assert!(migration(with(base.clone(), "rollout_salt", json!("   "))).is_none());
    assert!(migration(with(base.clone(), "rollout_salt", json!("a".repeat(64)))).is_some());
    assert!(migration(with(base.clone(), "rollout_salt", json!("a".repeat(65)))).is_none());
    assert!(migration(with(base.clone(), "rollout_salt", json!(5))).is_none());
    assert!(migration(without(base, "standalone_forwarding")).is_none());
}

#[test]
fn app_public_urls_are_kept_verbatim_after_trimming() {
    let mut document = with(
        deployed_official_document(),
        "app_public.branding.logo_url",
        json!("  https://cdn.fluxer.app/Logo.png  "),
    );
    document = with(
        document,
        "app_public.legal.guidelines_url",
        json!("javascript:alert(1)"),
    );
    document = with(
        document,
        "app_public.branding.trusted_domains",
        json!(["a.example"]),
    );
    document = with(
        document,
        "app_public.setup.account_identity_lock_reason",
        json!("admin_exists"),
    );
    let app_public = parse(&document).app_public.unwrap();
    assert_eq!(
        app_public.branding.logo_url,
        Some(Some("https://cdn.fluxer.app/Logo.png".to_owned()))
    );
    assert_eq!(app_public.legal.guidelines_url, Some(None));
    assert_eq!(
        app_public.branding.trusted_domains,
        Some(vec!["a.example".to_owned()])
    );
    assert_eq!(
        app_public.setup.account_identity_lock_reason,
        Some(Some("admin_exists".to_owned()))
    );
}
