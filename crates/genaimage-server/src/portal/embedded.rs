use axum::{
    body::Body,
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "../../apps/portal/dist/"]
struct Assets;

pub(super) fn serve(path: &str, headers: &HeaderMap) -> Response {
    let path = path.trim_start_matches('/');
    // Sidecars are representations, not public asset URLs.
    let identity = (!path.ends_with(".br") && !path.ends_with(".gz"))
        .then(|| Assets::get(path))
        .flatten();
    let (path, identity) = if let Some(asset) = identity {
        (path, asset)
    } else {
        (
            "index.html",
            Assets::get("index.html").expect("embedded portal index"),
        )
    };

    let preferences = EncodingPreferences::from_headers(headers);
    let candidates = [
        (preferences.br.or(preferences.wildcard).unwrap_or(0), "br"),
        (
            preferences.gzip.or(preferences.wildcard).unwrap_or(0),
            "gzip",
        ),
        (
            preferences
                .identity
                .unwrap_or_else(|| u16::from(preferences.wildcard != Some(0))),
            "identity",
        ),
    ];
    // Stable ordering breaks ties in favour of Brotli, then gzip.
    let mut candidates = candidates;
    candidates.sort_by_key(|&(quality, _)| std::cmp::Reverse(quality));
    for (quality, encoding) in candidates {
        if quality == 0 {
            continue;
        }
        let asset = match encoding {
            "br" => Assets::get(&format!("{path}.br")),
            "gzip" => Assets::get(&format!("{path}.gz")),
            _ => Some(identity.clone()),
        };
        if let Some(asset) = asset {
            let mut response = Response::builder()
                .header(header::VARY, "Accept-Encoding")
                .header(
                    header::CONTENT_TYPE,
                    mime_guess::from_path(path).first_or_octet_stream().as_ref(),
                )
                .header(header::CONTENT_LENGTH, asset.data.len())
                .header(
                    header::CACHE_CONTROL,
                    if hashed_asset(path) {
                        "public, max-age=31536000, immutable"
                    } else {
                        "no-cache"
                    },
                );
            if encoding != "identity" {
                response = response.header(header::CONTENT_ENCODING, encoding);
            }
            return response
                .body(Body::from(asset.data))
                .expect("valid portal response");
        }
    }
    (
        StatusCode::NOT_ACCEPTABLE,
        [
            (header::VARY, "Accept-Encoding"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
    )
        .into_response()
}

fn hashed_asset(path: &str) -> bool {
    // Vite's default output convention is assets/[name]-[hash:8].[ext].
    path.strip_prefix("assets/")
        .and_then(|path| path.rsplit_once('.'))
        .is_some_and(|(stem, _)| {
            let Some(start) = stem.len().checked_sub(9) else {
                return false;
            };
            let (separator, hash) = stem.as_bytes()[start..].split_at(1);
            separator == b"-"
                && hash
                    .iter()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        })
}

#[derive(Default)]
struct EncodingPreferences {
    br: Option<u16>,
    gzip: Option<u16>,
    identity: Option<u16>,
    wildcard: Option<u16>,
}

impl EncodingPreferences {
    fn from_headers(headers: &HeaderMap) -> Self {
        let mut preferences = Self::default();
        for value in headers
            .get_all(header::ACCEPT_ENCODING)
            .iter()
            .filter_map(|value| value.to_str().ok())
        {
            for entry in value.split(',') {
                let (encoding, weight) = entry.split_once(';').unwrap_or((entry, ""));
                let quality = if weight.is_empty() {
                    1000
                } else {
                    weight
                        .trim()
                        .split_once('=')
                        .filter(|(key, _)| key.eq_ignore_ascii_case("q"))
                        .and_then(|(_, value)| quality_weight(value))
                        .unwrap_or(0)
                };
                let target = match encoding.trim().to_ascii_lowercase().as_str() {
                    "br" => &mut preferences.br,
                    "gzip" => &mut preferences.gzip,
                    "identity" => &mut preferences.identity,
                    "*" => &mut preferences.wildcard,
                    _ => continue,
                };
                *target = Some(target.map_or(quality, |existing| existing.max(quality)));
            }
        }
        preferences
    }
}

fn quality_weight(weight: &str) -> Option<u16> {
    let (whole, fraction) = weight.split_once('.').unwrap_or((weight, ""));
    if fraction.len() > 3 || !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    match whole {
        "0" => Some(
            fraction
                .bytes()
                .zip([100, 10, 1])
                .map(|(digit, scale)| u16::from(digit - b'0') * scale)
                .sum(),
        ),
        "1" if fraction.bytes().all(|digit| digit == b'0') => Some(1000),
        _ => None,
    }
}
