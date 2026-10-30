// SPDX-License-Identifier: AGPL-3.0-or-later

use ::url::Url;

const DESKTOP_CEILING_CONNECT_SOURCES: [&str; 4] = ["https:", "wss:", "blob:", "data:"];
const DESKTOP_CEILING_IMAGE_SOURCES: [&str; 3] = ["https:", "blob:", "data:"];
const DESKTOP_CEILING_MEDIA_SOURCES: [&str; 3] = ["https:", "blob:", "data:"];

const FRAME_SOURCES: [&str; 2] = [
    "https://www.youtube.com/embed/",
    "https://www.youtube.com/s/player/",
];

const IMAGE_SOURCES: [&str; 5] = [
    "https://*.fluxer.app",
    "https://i.ytimg.com",
    "https://*.youtube.com",
    "https://*.fluxer.media",
    "https://fluxer.media",
];

const MEDIA_SOURCES: [&str; 4] = [
    "https://*.fluxer.app",
    "https://*.youtube.com",
    "https://*.fluxer.media",
    "https://fluxer.media",
];

const SCRIPT_SOURCES: [&str; 1] = ["https://*.fluxer.app"];

const STYLE_SOURCES: [&str; 3] = [
    "https://*.fluxer.app",
    "https://fonts.googleapis.com",
    "https://api.fonts.coollabs.io",
];

const FONT_SOURCES: [&str; 3] = [
    "https://*.fluxer.app",
    "https://fonts.gstatic.com",
    "https://api.fonts.coollabs.io",
];

const CONNECT_SOURCES: [&str; 7] = [
    "https://*.fluxer.app",
    "wss://*.fluxer.app",
    "https://*.fluxer.media",
    "wss://*.fluxer.media",
    "https://fluxer-uploads.ewr1.vultrobjects.com",
    "https://fluxerstatus.com",
    "https://fluxer.media",
];

const LOOPBACK_HTTP_SOURCES: [&str; 3] = [
    "http://localhost:*",
    "http://*.localhost:*",
    "http://127.0.0.1:*",
];

/// Tauri IPC transports; the Electron CSP has no equivalent.
const TAURI_IPC_CONNECT_SOURCES: [&str; 2] = ["ipc:", "http://ipc.localhost"];

const WORKER_SOURCES: [&str; 2] = ["https://*.fluxer.app", "blob:"];

const MANIFEST_SOURCES: [&str; 1] = ["https://*.fluxer.app"];

const SELF_SOURCE: &str = "'self'";
const NONE_SOURCE: &str = "'none'";
const DIRECTIVE_SEPARATOR: &str = "; ";

pub fn build_csp(nonce: &str, cleartext_instance_origins: &[String]) -> String {
    let mut cleartext_sources: Vec<String> = LOOPBACK_HTTP_SOURCES
        .iter()
        .map(|source| (*source).to_owned())
        .collect();
    extend_from(
        &mut cleartext_sources,
        cleartext_instance_origins
            .iter()
            .filter(|origin| is_cleartext_origin_source(origin))
            .map(String::as_str),
    );

    let mut directives: Vec<String> = Vec::new();
    directives.push(format!("default-src {SELF_SOURCE}"));

    let mut script = sources([
        SELF_SOURCE,
        &format!("'nonce-{nonce}'"),
        "'wasm-unsafe-eval'",
        "blob:",
    ]);
    extend_from(&mut script, SCRIPT_SOURCES);
    directives.push(directive("script-src", &script));

    let mut style = sources([SELF_SOURCE, "'unsafe-inline'"]);
    extend_from(&mut style, STYLE_SOURCES);
    directives.push(directive("style-src", &style));

    let mut image = sources([SELF_SOURCE, "blob:", "data:"]);
    extend_from(&mut image, IMAGE_SOURCES);
    extend_from(&mut image, DESKTOP_CEILING_IMAGE_SOURCES);
    extend_from(&mut image, cleartext_sources.iter().map(String::as_str));
    directives.push(directive("img-src", &image));

    let mut media = sources([SELF_SOURCE, "blob:"]);
    extend_from(&mut media, MEDIA_SOURCES);
    extend_from(&mut media, DESKTOP_CEILING_MEDIA_SOURCES);
    extend_from(&mut media, cleartext_sources.iter().map(String::as_str));
    directives.push(directive("media-src", &media));

    let mut font = sources([SELF_SOURCE, "data:"]);
    extend_from(&mut font, FONT_SOURCES);
    directives.push(directive("font-src", &font));

    let mut connect = sources([SELF_SOURCE, "blob:", "data:"]);
    extend_from(&mut connect, CONNECT_SOURCES);
    extend_from(&mut connect, DESKTOP_CEILING_CONNECT_SOURCES);
    extend_from(&mut connect, cleartext_sources.iter().map(String::as_str));
    let websocket_sources: Vec<String> = cleartext_sources
        .iter()
        .map(|source| format!("ws:{}", &source["http:".len()..]))
        .collect();
    extend_from(&mut connect, websocket_sources.iter().map(String::as_str));
    extend_from(&mut connect, TAURI_IPC_CONNECT_SOURCES);
    directives.push(directive("connect-src", &connect));

    let mut frame = sources([SELF_SOURCE]);
    extend_from(&mut frame, FRAME_SOURCES);
    directives.push(directive("frame-src", &frame));

    let mut worker = sources([SELF_SOURCE, "blob:"]);
    extend_from(&mut worker, WORKER_SOURCES);
    directives.push(directive("worker-src", &worker));

    let mut manifest = sources([SELF_SOURCE]);
    extend_from(&mut manifest, MANIFEST_SOURCES);
    directives.push(directive("manifest-src", &manifest));

    directives.push(format!("object-src {NONE_SOURCE}"));
    directives.push(format!("base-uri {SELF_SOURCE}"));
    directives.push(format!("frame-ancestors {NONE_SOURCE}"));

    directives.join(DIRECTIVE_SEPARATOR)
}

fn sources<const N: usize>(initial: [&str; N]) -> Vec<String> {
    initial.iter().map(|source| (*source).to_owned()).collect()
}

fn directive(name: &str, sources: &[String]) -> String {
    format!("{name} {}", sources.join(" "))
}

fn is_cleartext_origin_source(origin: &str) -> bool {
    let Ok(url) = Url::parse(origin) else {
        return false;
    };
    url.scheme() == "http"
        && url.origin().ascii_serialization() == origin
        && !url.host_str().is_some_and(|host| host.starts_with('['))
}

fn extend_from<'a>(target: &mut Vec<String>, defaults: impl IntoIterator<Item = &'a str>) {
    for source in defaults {
        if !target.iter().any(|existing| existing == source) {
            target.push(source.to_owned());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEFAULT_CSP: &str = concat!(
        "default-src 'self'; ",
        "script-src 'self' 'nonce-N0NCE' 'wasm-unsafe-eval' blob: https://*.fluxer.app; ",
        "style-src 'self' 'unsafe-inline' https://*.fluxer.app https://fonts.googleapis.com https://api.fonts.coollabs.io; ",
        "img-src 'self' blob: data: https://*.fluxer.app https://i.ytimg.com https://*.youtube.com https://*.fluxer.media https://fluxer.media https: http://localhost:* http://*.localhost:* http://127.0.0.1:*; ",
        "media-src 'self' blob: https://*.fluxer.app https://*.youtube.com https://*.fluxer.media https://fluxer.media https: data: http://localhost:* http://*.localhost:* http://127.0.0.1:*; ",
        "font-src 'self' data: https://*.fluxer.app https://fonts.gstatic.com https://api.fonts.coollabs.io; ",
        "connect-src 'self' blob: data: https://*.fluxer.app wss://*.fluxer.app https://*.fluxer.media wss://*.fluxer.media https://fluxer-uploads.ewr1.vultrobjects.com https://fluxerstatus.com https://fluxer.media https: wss: http://localhost:* http://*.localhost:* http://127.0.0.1:* ws://localhost:* ws://*.localhost:* ws://127.0.0.1:* ipc: http://ipc.localhost; ",
        "frame-src 'self' https://www.youtube.com/embed/ https://www.youtube.com/s/player/; ",
        "worker-src 'self' blob: https://*.fluxer.app; ",
        "manifest-src 'self' https://*.fluxer.app; ",
        "object-src 'none'; base-uri 'self'; frame-ancestors 'none'",
    );

    fn origins(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn default_policy_matches_reference() {
        assert_eq!(build_csp("N0NCE", &[]), DEFAULT_CSP);
    }

    #[test]
    fn cleartext_instance_origins_extend_img_media_connect() {
        let csp = build_csp("n", &origins(&["http://a.test", "http://192.168.1.5:8080"]));
        assert!(csp.contains(" http://127.0.0.1:* http://a.test http://192.168.1.5:8080; "));
        assert!(csp.contains(
            " ws://127.0.0.1:* ws://a.test ws://192.168.1.5:8080 ipc: http://ipc.localhost; frame-src"
        ));
        assert_eq!(csp.matches("http://a.test").count(), 3);
    }

    #[test]
    fn invalid_cleartext_origins_are_dropped() {
        for origin in [
            "http://a.test/",
            "http://A.test",
            "https://a.test",
            "http://[::1]:3000",
            "http://a.test:80",
            "http://a.test/path",
            "not a url",
            "",
        ] {
            assert_eq!(
                build_csp("N0NCE", &origins(&[origin])),
                DEFAULT_CSP,
                "{origin:?}"
            );
        }
    }

    #[test]
    fn duplicate_and_loopback_origins_are_deduped() {
        let csp = build_csp(
            "N0NCE",
            &origins(&["http://x.test", "http://x.test", "http://localhost:*"]),
        );
        assert_eq!(csp.matches("http://x.test").count(), 3);
        assert_eq!(csp.matches("http://localhost:*").count(), 3);
        assert_eq!(csp.matches("ws://localhost:*").count(), 1);
    }
}
