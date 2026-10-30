// SPDX-License-Identifier: AGPL-3.0-or-later

//! TLS client trust for outbound HTTP: OS roots ∪ Mozilla roots ∪ (Windows)
//! self-issued serverAuth roots from the CA intermediate stores.

use std::fmt;
use std::sync::{Arc, OnceLock};

use rustls::pki_types::CertificateDer;

#[derive(Clone, Debug)]
pub struct TrustError(String);

impl fmt::Display for TrustError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TLS client configuration failed: {}", self.0)
    }
}

impl std::error::Error for TrustError {}

static CLIENT_CONFIG: OnceLock<Result<Arc<rustls::ClientConfig>, TrustError>> = OnceLock::new();

/// Shared rustls client configuration (ring provider, ALPN `h2`, `http/1.1`).
pub fn client_config() -> Result<Arc<rustls::ClientConfig>, TrustError> {
    CLIENT_CONFIG.get_or_init(build_client_config).clone()
}

fn build_client_config() -> Result<Arc<rustls::ClientConfig>, TrustError> {
    let native = rustls_native_certs::load_native_certs();
    if !native.errors.is_empty() {
        tracing::warn!(errors = ?native.errors, "could not read every native certificate store");
    }
    let mut roots = rustls::RootCertStore::empty();
    roots.add_parsable_certificates(native.certs);
    roots.add_parsable_certificates(intermediate_store_roots());
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let mut config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|error| TrustError(error.to_string()))?
    .with_root_certificates(roots)
    .with_no_client_auth();
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

// `der_element`, `is_self_issued` and `intermediate_store_roots` are copied from
// fluxer_desktop/native/gateway-socket/src/lib.rs so both TLS clients trust the
// same Windows roots. Keep them in sync.

#[cfg(windows)]
fn der_element(input: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = input.split_first()?;
    let (&first, rest) = rest.split_first()?;
    let (length, rest) = if first & 0x80 == 0 {
        (usize::from(first), rest)
    } else {
        let size = usize::from(first & 0x7f);
        if size == 0 || size > 4 || rest.len() < size {
            return None;
        }
        let (bytes, rest) = rest.split_at(size);
        (
            bytes
                .iter()
                .fold(0usize, |length, &byte| (length << 8) | usize::from(byte)),
            rest,
        )
    };
    if rest.len() < length {
        return None;
    }
    let (content, rest) = rest.split_at(length);
    Some((tag, content, rest))
}

#[cfg(windows)]
fn is_self_issued(certificate: &[u8]) -> bool {
    let Some((0x30, certificate, [])) = der_element(certificate) else {
        return false;
    };
    let Some((0x30, tbs, _)) = der_element(certificate) else {
        return false;
    };
    let mut fields = std::iter::successors(der_element(tbs), |&(_, _, rest)| der_element(rest))
        .map(|(tag, content, _)| (tag, content));
    let mut serial = fields.next();
    if matches!(serial, Some((0xa0, _))) {
        serial = fields.next();
    }
    let (
        Some((0x02, _)),
        Some((0x30, _)),
        Some((0x30, issuer)),
        Some((0x30, _)),
        Some((0x30, subject)),
    ) = (
        serial,
        fields.next(),
        fields.next(),
        fields.next(),
        fields.next(),
    )
    else {
        return false;
    };
    issuer == subject
}

#[cfg(windows)]
fn intermediate_store_roots() -> Vec<CertificateDer<'static>> {
    use schannel::{cert_context::ValidUses, cert_store::CertStore};
    use std::collections::BTreeSet;

    const SERVER_AUTH: &str = "1.3.6.1.5.5.7.3.1";
    const ANY_EXTENDED_KEY_USAGE: &str = "2.5.29.37.0";
    let store_certificates = |name: &str| {
        [
            CertStore::open_current_user(name),
            CertStore::open_local_machine(name),
        ]
        .into_iter()
        .flatten()
        .flat_map(|store| store.certs().collect::<Vec<_>>())
    };
    let disallowed: BTreeSet<Vec<u8>> = store_certificates("Disallowed")
        .map(|certificate| certificate.to_der().to_vec())
        .collect();
    store_certificates("CA")
        .filter(|certificate| {
            certificate.is_time_valid().unwrap_or(false)
                && match certificate.valid_uses() {
                    Ok(ValidUses::All) => true,
                    Ok(ValidUses::Oids(oids)) => oids
                        .iter()
                        .any(|oid| oid == SERVER_AUTH || oid == ANY_EXTENDED_KEY_USAGE),
                    Err(_) => false,
                }
        })
        .map(|certificate| certificate.to_der().to_vec())
        .filter(|der| is_self_issued(der) && !disallowed.contains(der))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(CertificateDer::from)
        .collect()
}

#[cfg(not(windows))]
fn intermediate_store_roots() -> Vec<CertificateDer<'static>> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_is_cached_and_offers_h2_then_http11() {
        let first = client_config().unwrap();
        let second = client_config().unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(
            first.alpn_protocols,
            vec![b"h2".to_vec(), b"http/1.1".to_vec()]
        );
    }
}
