// SPDX-License-Identifier: AGPL-3.0-or-later

//! Upstream content decoding, `decodedMessageStream` and `createDecoder` of
//! LocalAppProxyClient.ts over a buffered body. The decoded size is capped so a small compressed
//! body cannot expand without bound, and each decoded chunk is reserved from the response buffer
//! budget before it is kept.

use std::io::{self, Read};

use flate2::read::{MultiGzDecoder, ZlibDecoder};
use tauri::http::{HeaderMap, header};

use super::budget::BudgetError;
use crate::js::js_trim;

const BROTLI_BUFFER_BYTES: usize = 64 * 1024;
const DECODE_CHUNK_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContentEncoding {
    /// `zlib.createGunzip`, which also reads concatenated members.
    Gzip,
    /// `zlib.createInflate`: zlib-wrapped deflate.
    Deflate,
    Brotli,
}

#[derive(Debug)]
pub enum DecodeError {
    Unsupported(String),
    TooLarge,
    Invalid(io::Error),
    Budget(BudgetError),
}

/// `contentEncoding` + `createDecoder`: `None` for an absent, blank or `identity` encoding.
/// Repeated headers join with `, ` as Node joined them, so a stacked encoding is unsupported.
pub fn content_encoding(headers: &HeaderMap) -> Result<Option<ContentEncoding>, DecodeError> {
    let joined = headers
        .get_all(header::CONTENT_ENCODING)
        .iter()
        .map(|value| String::from_utf8_lossy(value.as_bytes()))
        .collect::<Vec<_>>()
        .join(", ");
    let normalized = js_trim(&joined).to_lowercase();
    match normalized.as_str() {
        "" | "identity" => Ok(None),
        "gzip" | "x-gzip" => Ok(Some(ContentEncoding::Gzip)),
        "deflate" => Ok(Some(ContentEncoding::Deflate)),
        "br" => Ok(Some(ContentEncoding::Brotli)),
        _ => Err(DecodeError::Unsupported(normalized)),
    }
}

/// Decodes `body`, failing once the output would exceed `limit` bytes. `reserve` is handed the
/// size of each decoded chunk before the chunk is kept.
pub fn decode(
    encoding: ContentEncoding,
    body: &[u8],
    limit: usize,
    mut reserve: impl FnMut(usize) -> Result<(), BudgetError>,
) -> Result<Vec<u8>, DecodeError> {
    let mut decoder: Box<dyn Read + '_> = match encoding {
        ContentEncoding::Gzip => Box::new(MultiGzDecoder::new(body)),
        ContentEncoding::Deflate => Box::new(ZlibDecoder::new(body)),
        ContentEncoding::Brotli => Box::new(brotli_decompressor::Decompressor::new(
            body,
            BROTLI_BUFFER_BYTES,
        )),
    };
    let mut decoded = Vec::new();
    let mut chunk = vec![0u8; DECODE_CHUNK_BYTES];
    loop {
        let read = match decoder.read(&mut chunk) {
            Ok(0) => return Ok(decoded),
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(DecodeError::Invalid(error)),
        };
        if decoded.len() + read > limit {
            return Err(DecodeError::TooLarge);
        }
        reserve(read).map_err(DecodeError::Budget)?;
        decoded.extend_from_slice(&chunk[..read]);
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use flate2::Compression;
    use flate2::write::{DeflateEncoder, GzEncoder, ZlibEncoder};
    use tauri::http::HeaderValue;

    use super::*;

    const TEXT: &[u8] = b"{\"hello\":\"world\"}";
    /// `zlib.brotliCompressSync('{"hello":"world"}')` from Node.
    const BROTLI_TEXT: &[u8] = &[
        0x0b, 0x08, 0x80, 0x7b, 0x22, 0x68, 0x65, 0x6c, 0x6c, 0x6f, 0x22, 0x3a, 0x22, 0x77, 0x6f,
        0x72, 0x6c, 0x64, 0x22, 0x7d, 0x03,
    ];
    /// `zlib.brotliCompressSync(Buffer.alloc(4 * 1024 * 1024))` from Node: 14 bytes, 4 MiB out.
    const BROTLI_ZEROS: &[u8] = &[
        0x9b, 0xff, 0xff, 0x3f, 0xf8, 0x27, 0x00, 0xe2, 0xb1, 0x40, 0x20, 0xf7, 0xfe, 0x07,
    ];

    fn gzip(data: &[u8]) -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    }

    fn unbudgeted(_bytes: usize) -> Result<(), BudgetError> {
        Ok(())
    }

    fn zlib(data: &[u8]) -> Vec<u8> {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    }

    fn encoding_of(values: &[&str]) -> Result<Option<ContentEncoding>, DecodeError> {
        let mut headers = HeaderMap::new();
        for value in values {
            headers.append(
                header::CONTENT_ENCODING,
                HeaderValue::from_str(value).unwrap(),
            );
        }
        content_encoding(&headers)
    }

    #[test]
    fn content_encoding_names_are_normalised() {
        assert!(matches!(encoding_of(&[]), Ok(None)));
        assert!(matches!(encoding_of(&[""]), Ok(None)));
        assert!(matches!(encoding_of(&[" Identity "]), Ok(None)));
        assert!(matches!(
            encoding_of(&["GZIP"]),
            Ok(Some(ContentEncoding::Gzip))
        ));
        assert!(matches!(
            encoding_of(&["x-gzip"]),
            Ok(Some(ContentEncoding::Gzip))
        ));
        assert!(matches!(
            encoding_of(&["deflate"]),
            Ok(Some(ContentEncoding::Deflate))
        ));
        assert!(matches!(
            encoding_of(&["br"]),
            Ok(Some(ContentEncoding::Brotli))
        ));
    }

    #[test]
    fn unknown_and_stacked_encodings_are_unsupported() {
        for values in [
            &["zstd"][..],
            &["gzip, br"],
            &["gzip", "gzip"],
            &["compress"],
        ] {
            assert!(
                matches!(encoding_of(values), Err(DecodeError::Unsupported(_))),
                "{values:?}"
            );
        }
    }

    #[test]
    fn each_encoding_decodes() {
        assert_eq!(
            decode(ContentEncoding::Gzip, &gzip(TEXT), 1024, unbudgeted).unwrap(),
            TEXT
        );
        assert_eq!(
            decode(ContentEncoding::Deflate, &zlib(TEXT), 1024, unbudgeted).unwrap(),
            TEXT
        );
        assert_eq!(
            decode(ContentEncoding::Brotli, BROTLI_TEXT, 1024, unbudgeted).unwrap(),
            TEXT
        );
    }

    #[test]
    fn concatenated_gzip_members_decode_like_gunzip() {
        let mut body = gzip(b"first ");
        body.extend(gzip(b"second"));
        assert_eq!(
            decode(ContentEncoding::Gzip, &body, 1024, unbudgeted).unwrap(),
            b"first second"
        );
    }

    #[test]
    fn deflate_requires_the_zlib_wrapper_like_create_inflate() {
        let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(TEXT).unwrap();
        let raw = encoder.finish().unwrap();
        assert!(matches!(
            decode(ContentEncoding::Deflate, &raw, 1024, unbudgeted),
            Err(DecodeError::Invalid(_))
        ));
    }

    #[test]
    fn corrupt_and_truncated_bodies_fail() {
        let gzipped = gzip(TEXT);
        assert!(matches!(
            decode(
                ContentEncoding::Gzip,
                &gzipped[..gzipped.len() - 4],
                1024,
                unbudgeted
            ),
            Err(DecodeError::Invalid(_))
        ));
        assert!(matches!(
            decode(ContentEncoding::Gzip, b"not gzip", 1024, unbudgeted),
            Err(DecodeError::Invalid(_))
        ));
        assert!(matches!(
            decode(
                ContentEncoding::Brotli,
                &BROTLI_TEXT[..BROTLI_TEXT.len() - 1],
                1024,
                unbudgeted
            ),
            Err(DecodeError::Invalid(_))
        ));
    }

    #[test]
    fn decoded_output_is_capped() {
        let zeros = vec![0u8; 4 * 1024 * 1024];
        let bomb = gzip(&zeros);
        assert!(bomb.len() < 64 * 1024);
        assert!(matches!(
            decode(ContentEncoding::Gzip, &bomb, 1024 * 1024, unbudgeted),
            Err(DecodeError::TooLarge)
        ));
        assert!(matches!(
            decode(
                ContentEncoding::Deflate,
                &zlib(&zeros),
                1024 * 1024,
                unbudgeted
            ),
            Err(DecodeError::TooLarge)
        ));
        assert!(matches!(
            decode(
                ContentEncoding::Brotli,
                BROTLI_ZEROS,
                1024 * 1024,
                unbudgeted
            ),
            Err(DecodeError::TooLarge)
        ));
        assert_eq!(
            decode(ContentEncoding::Gzip, &bomb, zeros.len(), unbudgeted)
                .unwrap()
                .len(),
            zeros.len()
        );
        assert_eq!(
            decode(
                ContentEncoding::Brotli,
                BROTLI_ZEROS,
                zeros.len(),
                unbudgeted
            )
            .unwrap()
            .len(),
            zeros.len()
        );
    }

    #[test]
    fn every_kept_chunk_is_reserved_first() {
        let zeros = vec![0u8; 4 * 1024 * 1024];
        let mut reserved = 0;
        let decoded = decode(ContentEncoding::Gzip, &gzip(&zeros), zeros.len(), |bytes| {
            reserved += bytes;
            Ok(())
        })
        .unwrap();
        assert_eq!(decoded.len(), zeros.len());
        assert_eq!(reserved, zeros.len());
    }

    #[test]
    fn a_refused_reservation_fails_the_decode() {
        let zeros = vec![0u8; 1024 * 1024];
        let mut reserved = 0;
        let refused = decode(ContentEncoding::Gzip, &gzip(&zeros), zeros.len(), |bytes| {
            if reserved + bytes > 100 * 1024 {
                return Err(BudgetError::Timeout);
            }
            reserved += bytes;
            Ok(())
        });
        assert!(matches!(
            refused,
            Err(DecodeError::Budget(BudgetError::Timeout))
        ));
    }
}
