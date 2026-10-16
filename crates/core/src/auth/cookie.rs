use axum::http::{HeaderMap, HeaderValue, header};

/// Name of the access-token cookie.
pub const SESSION_COOKIE: &str = "outturn_session";

/// Name of the refresh-token cookie. Scoped by Path so it is sent only to the
/// refresh endpoint rather than on every request.
pub const REFRESH_COOKIE: &str = "outturn_refresh";

/// Path the refresh cookie is confined to.
pub const REFRESH_PATH: &str = "/v1/session/refresh";

/// Builds the Set-Cookie header for a freshly minted token.
///
/// HttpOnly keeps the token out of reach of page JavaScript, so an XSS or a
/// compromised dependency cannot exfiltrate it. SameSite=Lax blocks the
/// cross-site POSTs that would otherwise be possible now that the browser
/// attaches this automatically, which is what removes the need for a separate
/// CSRF token on a same-site app.
///
/// `max_age` should match the token's own lifetime: the cookie expiring first
/// would log the user out early, and expiring later just means a rejected
/// token on the next call.
pub fn session_cookie(token: &str, max_age_secs: u64) -> HeaderValue {
    // Secure is unconditional: browsers treat localhost as a trustworthy
    // origin and accept Secure cookies there even over plain HTTP, so dev
    // needs no exception.
    let value = format!(
        "{SESSION_COOKIE}={token}; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age={max_age_secs}"
    );
    HeaderValue::from_str(&value).expect("cookie value is header-safe")
}

/// Clears the session cookie. Attributes must match those it was set with, or
/// the browser keeps the original.
pub fn clear_session_cookie() -> HeaderValue {
    HeaderValue::from_static("outturn_session=; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age=0")
}

/// Reads the session token from the Cookie header.
pub fn session_from_cookies(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .filter_map(|c| c.trim().split_once('='))
        .find(|(name, _)| *name == SESSION_COOKIE)
        .map(|(_, value)| value)
}

/// Builds the Set-Cookie header for a refresh token.
///
/// Path-scoped to the refresh endpoint: a token that is only sent there cannot
/// be leaked by an unrelated request, and narrows what a proxy or log sees.
pub fn refresh_cookie(token: &str, max_age_secs: u64) -> HeaderValue {
    let value = format!(
        "{REFRESH_COOKIE}={token}; HttpOnly; Secure; SameSite=Lax; \
         Path={REFRESH_PATH}; Max-Age={max_age_secs}"
    );
    HeaderValue::from_str(&value).expect("cookie value is header-safe")
}

pub fn clear_refresh_cookie() -> HeaderValue {
    HeaderValue::from_static(
        "outturn_refresh=; HttpOnly; Secure; SameSite=Lax; Path=/v1/session/refresh; Max-Age=0",
    )
}

/// Reads the refresh token from the Cookie header.
pub fn refresh_from_cookies(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .filter_map(|c| c.trim().split_once('='))
        .find(|(name, _)| *name == REFRESH_COOKIE)
        .map(|(_, value)| value)
}
