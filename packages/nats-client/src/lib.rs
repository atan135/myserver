//! Shared NATS connection helpers for MyServer Rust services.

use percent_encoding::percent_decode_str;
use url::Url;

/// Connect to NATS, promoting a token-only URL userinfo value to NATS token authentication.
///
/// Production secrets encode Core NATS tokens as `nats://<token>@host:port` so they remain a
/// single connection setting. `async_nats` treats URL userinfo as username/password credentials,
/// not a token, so pass it explicitly through `ConnectOptions` after removing it from the server
/// address. URLs without token-only userinfo retain the library's native behavior.
pub async fn connect(nats_url: &str) -> Result<async_nats::Client, async_nats::ConnectError> {
    if let Some((server, token)) = token_connection_parts(nats_url) {
        return async_nats::ConnectOptions::with_token(token)
            .connect(server)
            .await;
    }

    async_nats::connect(nats_url).await
}

/// Extract the server address and token from `nats://<token>@host:port`.
///
/// User/password URLs are deliberately left alone so existing local or external configurations
/// that use a different authentication mechanism continue through `async_nats` unchanged.
pub fn token_connection_parts(nats_url: &str) -> Option<(String, String)> {
    let mut parsed = Url::parse(nats_url).ok()?;
    if parsed.username().is_empty() || parsed.password().is_some() {
        return None;
    }

    let token = percent_decode_str(parsed.username())
        .decode_utf8()
        .ok()?
        .into_owned();
    if token.is_empty() {
        return None;
    }

    parsed.set_username("").ok()?;
    Some((parsed.into(), token))
}

#[cfg(test)]
mod tests {
    use super::token_connection_parts;
    use url::Url;

    #[test]
    fn extracts_token_only_userinfo_and_removes_it_from_server_address() {
        let (server, token) = token_connection_parts("nats://secret-token@nats:4222").unwrap();
        let server = Url::parse(&server).unwrap();

        assert_eq!(token, "secret-token");
        assert_eq!(server.scheme(), "nats");
        assert_eq!(server.host_str(), Some("nats"));
        assert_eq!(server.port(), Some(4222));
        assert_eq!(server.username(), "");
        assert_eq!(server.password(), None);
    }

    #[test]
    fn decodes_url_encoded_token() {
        let (_, token) = token_connection_parts("nats://token%2Fwith%2Bchars@nats:4222").unwrap();

        assert_eq!(token, "token/with+chars");
    }

    #[test]
    fn leaves_plain_and_user_password_urls_for_native_client_handling() {
        assert_eq!(token_connection_parts("nats://127.0.0.1:4222"), None);
        assert_eq!(
            token_connection_parts("nats://username:password@127.0.0.1:4222"),
            None
        );
    }
}
