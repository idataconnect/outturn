//! A second check behind `SameSite=Lax` for requests a cookie authenticates.
//!
//! `Lax` already keeps a cross-site POST from carrying the session cookie in
//! current browsers, but it is one defense resting on one attribute, and
//! "same site" is wider than "same origin". Every route here takes JSON, which
//! a plain cross-site form cannot send; this is the cheap insurance for the day
//! one does not.
//!
//! Only a request that carries a cookie and no `Authorization` header is
//! looked at, and only when it can change something. A bearer is not
//! attached by a browser on its own, so a request bearing one was not forged
//! by a page the caller happened to visit.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

/// The origins allowed to call with credentials: `OUTTURN_CORS_ORIGINS`, the
/// same list the CORS layer answers to.
#[derive(Debug, Clone)]
pub struct AllowedOrigins(Vec<String>);

impl AllowedOrigins {
    /// From the comma-separated form the environment variable takes.
    pub fn parse(list: &str) -> Self {
        Self(
            list.split(',')
                .map(|o| o.trim().trim_end_matches('/').to_ascii_lowercase())
                .filter(|o| !o.is_empty())
                .collect(),
        )
    }

    /// Whether to turn this request away as one a page on another site may
    /// have made.
    pub fn refuses(&self, method: &Method, headers: &HeaderMap) -> bool {
        if matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS) {
            return false;
        }
        if !headers.contains_key(header::COOKIE) || headers.contains_key(header::AUTHORIZATION) {
            return false;
        }

        // The browser's own account of the request, which a page cannot set.
        match header_str(headers, "sec-fetch-site") {
            Some("cross-site") => return true,
            // Nothing is more trustworthy than the page's own origin, and
            // `none` is the user typing the address. A same-origin UI served
            // by this API needs no entry in the CORS list.
            Some("same-origin" | "none") => return false,
            _ => {}
        }

        // An older browser, or a same-site neighbor: fall back on Origin.
        // Absent, it is a client that is not a browser page.
        let Some(origin) = header_str(headers, "origin") else {
            return false;
        };
        let origin = origin.trim_end_matches('/').to_ascii_lowercase();
        if self.0.contains(&origin) {
            return false;
        }
        let same_host = header_str(headers, "host").is_some_and(|host| {
            origin
                .split_once("://")
                .is_some_and(|(_, authority)| authority.eq_ignore_ascii_case(host))
        });
        !same_host
    }
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name)?.to_str().ok()
}

/// The middleware: 403 for a request [`AllowedOrigins::refuses`].
pub async fn guard(
    State(allowed): State<Arc<AllowedOrigins>>,
    request: Request,
    next: Next,
) -> Response {
    if allowed.refuses(request.method(), request.headers()) {
        return (
            StatusCode::FORBIDDEN,
            "cross-site request refused".to_string(),
        )
            .into_response();
    }
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allowed() -> AllowedOrigins {
        AllowedOrigins::parse("http://localhost:3000, https://app.example.com/")
    }

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::COOKIE, "outturn_session=x".parse().unwrap());
        for (k, v) in pairs {
            h.insert(
                axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                v.parse().unwrap(),
            );
        }
        h
    }

    #[test]
    fn a_cross_site_post_with_a_cookie_is_refused() {
        let a = allowed();
        let h = headers(&[("sec-fetch-site", "cross-site")]);
        assert!(a.refuses(&Method::POST, &h));
        assert!(a.refuses(&Method::DELETE, &h));
        assert!(!a.refuses(&Method::GET, &h));
        assert!(!a.refuses(&Method::OPTIONS, &h));
    }

    #[test]
    fn a_post_from_the_ui_origin_is_not() {
        let a = allowed();
        // The Vite dev proxy forwards the browser's own headers.
        assert!(!a.refuses(
            &Method::POST,
            &headers(&[
                ("sec-fetch-site", "same-origin"),
                ("origin", "http://localhost:3000")
            ])
        ));
        assert!(!a.refuses(
            &Method::POST,
            &headers(&[("origin", "http://localhost:3000")])
        ));
        assert!(!a.refuses(
            &Method::POST,
            &headers(&[("origin", "https://APP.example.com")])
        ));
    }

    #[test]
    fn an_unlisted_origin_is_refused_unless_it_is_this_host() {
        let a = allowed();
        let evil = headers(&[("origin", "https://evil.example")]);
        assert!(a.refuses(&Method::POST, &evil));
        let sibling = headers(&[
            ("sec-fetch-site", "same-site"),
            ("origin", "https://evil.example"),
        ]);
        assert!(a.refuses(&Method::POST, &sibling));
        let own = headers(&[
            ("origin", "https://api.example.org"),
            ("host", "api.example.org"),
        ]);
        assert!(!a.refuses(&Method::POST, &own));
    }

    #[test]
    fn requests_that_are_not_cookie_authenticated_are_left_alone() {
        let a = allowed();
        let mut bearer = headers(&[("sec-fetch-site", "cross-site")]);
        bearer.insert(header::AUTHORIZATION, "Bearer t".parse().unwrap());
        assert!(!a.refuses(&Method::POST, &bearer));

        let mut no_cookie = headers(&[("origin", "https://evil.example")]);
        no_cookie.remove(header::COOKIE);
        assert!(!a.refuses(&Method::POST, &no_cookie));

        // No Origin and no Sec-Fetch-Site: not a browser page.
        assert!(!a.refuses(&Method::POST, &headers(&[])));
    }
}
