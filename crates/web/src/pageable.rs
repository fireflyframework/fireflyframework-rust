// Copyright 2026 Firefly Software Foundation.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! The [`PageRequest`] argument resolver — Spring MVC's `Pageable` parameter,
//! parsed straight out of the request's `?page=&size=&sort=` query string.
//!
//! A handler takes `PageRequest(pageable)` and the framework binds a
//! [`firefly_data::Pageable`] from the query, so the controller never re-parses
//! pagination by hand. It mirrors Spring Data Web's `PageableHandlerMethodArgumentResolver`.

use axum::extract::FromRequestParts;
use axum::response::{IntoResponse, Response};
use firefly_data::{Direction, Order, Pageable, RequestSort};
use firefly_kernel::FireflyError;
use http::request::Parts;

use crate::problem::WebError;

/// The default 1-based page when `?page=` is absent.
const DEFAULT_PAGE: usize = 1;
/// The default page size when `?size=` is absent — Spring Data's default.
const DEFAULT_SIZE: usize = 20;
/// An upper bound on `?size=` so a client can't request an unbounded page —
/// Spring Data's `maxPageSize` (default 2000).
const MAX_SIZE: usize = 2000;

/// An argument resolver that binds a [`firefly_data::Pageable`] from the
/// request's pagination query parameters — the Rust analog of accepting a
/// Spring `Pageable` method parameter.
///
/// Recognised query parameters:
/// - `page` — the **1-based** page number (default `1`);
/// - `size` — the page size (default `20`, capped at `2000`);
/// - `sort` — a sort order `property[,asc|desc]` (direction defaults to `asc`);
///   **repeatable** for a multi-key sort, e.g.
///   `?sort=owner,desc&sort=balance,asc`.
///
/// ```ignore
/// use firefly::web::PageRequest;
///
/// // GET /wallets?page=2&size=10&sort=owner,desc
/// async fn list(PageRequest(pageable): PageRequest) -> WebResult<Json<Page<Wallet>>> {
///     let page = service.find(pageable).await?;   // pageable.page == 2, size == 10
///     Ok(Json(page))
/// }
/// ```
///
/// A non-numeric `page`/`size`, a zero page/size, or a malformed `sort` rejects
/// with a **400** `application/problem+json` before the handler runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageRequest(pub Pageable);

impl PageRequest {
    /// Consumes the wrapper, returning the resolved [`Pageable`].
    pub fn into_inner(self) -> Pageable {
        self.0
    }

    /// Borrows the resolved [`Pageable`].
    pub fn pageable(&self) -> &Pageable {
        &self.0
    }

    /// Parses a query string into a [`Pageable`] (the resolver's core, factored
    /// out so it is unit-testable without a live request).
    fn parse(query: &str) -> Result<Pageable, FireflyError> {
        let mut page = DEFAULT_PAGE;
        let mut size = DEFAULT_SIZE;
        let mut orders: Vec<Order> = Vec::new();

        for (key, value) in form_urlencoded::parse(query.as_bytes()) {
            match key.as_ref() {
                "page" => {
                    page = value.parse().map_err(|_| {
                        FireflyError::bad_request(format!("`page` must be a number, got {value:?}"))
                    })?;
                }
                "size" => {
                    size = value.parse().map_err(|_| {
                        FireflyError::bad_request(format!("`size` must be a number, got {value:?}"))
                    })?;
                }
                "sort" => orders.push(parse_order(&value)?),
                // Unknown parameters are ignored, as Spring's resolver does.
                _ => {}
            }
        }

        if size > MAX_SIZE {
            size = MAX_SIZE;
        }
        let sort = RequestSort::of(orders);
        Pageable::of(page, size, sort).map_err(|e| FireflyError::bad_request(e.to_string()))
    }
}

/// Parses one `sort` value — `property` or `property,asc` / `property,desc`.
fn parse_order(raw: &str) -> Result<Order, FireflyError> {
    let mut parts = raw.splitn(2, ',');
    let property = parts.next().unwrap_or("").trim();
    if property.is_empty() {
        return Err(FireflyError::bad_request(
            "`sort` is missing a property name",
        ));
    }
    let direction = match parts.next().map(|d| d.trim().to_ascii_lowercase()) {
        None => Direction::Asc,
        Some(d) if d == "asc" => Direction::Asc,
        Some(d) if d == "desc" => Direction::Desc,
        Some(d) => {
            return Err(FireflyError::bad_request(format!(
                "`sort` direction must be `asc` or `desc`, got {d:?}"
            )))
        }
    };
    Ok(Order::new(property.to_owned(), direction))
}

#[axum::async_trait]
impl<S> FromRequestParts<S> for PageRequest
where
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let query = parts.uri.query().unwrap_or("");
        match PageRequest::parse(query) {
            Ok(pageable) => Ok(PageRequest(pageable)),
            Err(err) => Err(WebError::from(err).into_response()),
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::routing::get;
    use axum::Router;
    use firefly_data::Direction;
    use firefly_kernel::PROBLEM_CONTENT_TYPE;
    use http::{header, Request, StatusCode};
    use tower::ServiceExt;

    use super::PageRequest;

    async fn echo(PageRequest(p): PageRequest) -> String {
        let sorts: Vec<String> = p
            .sort
            .orders
            .iter()
            .map(|o| {
                format!(
                    "{}:{}",
                    o.property,
                    match o.direction {
                        Direction::Asc => "asc",
                        Direction::Desc => "desc",
                    }
                )
            })
            .collect();
        format!("{}|{}|{}", p.page, p.size, sorts.join(","))
    }

    async fn get_line(uri: &str) -> (StatusCode, Option<String>, String) {
        let res = Router::new()
            .route("/", get(echo))
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = res.status();
        let ct = res
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let bytes = http_body_util::BodyExt::collect(res.into_body())
            .await
            .unwrap()
            .to_bytes();
        (status, ct, String::from_utf8(bytes.to_vec()).unwrap())
    }

    #[tokio::test]
    async fn defaults_when_absent() {
        let (status, _ct, body) = get_line("/").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "1|20|", "page 1, size 20, unsorted");
    }

    #[tokio::test]
    async fn parses_page_size_and_multi_key_sort() {
        let (status, _ct, body) =
            get_line("/?page=3&size=15&sort=owner,desc&sort=balance,asc").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "3|15|owner:desc,balance:asc");
    }

    #[tokio::test]
    async fn sort_direction_defaults_to_asc() {
        let (_status, _ct, body) = get_line("/?sort=owner").await;
        assert_eq!(body, "1|20|owner:asc");
    }

    #[tokio::test]
    async fn caps_oversized_page() {
        let (_status, _ct, body) = get_line("/?size=999999").await;
        assert_eq!(body, "1|2000|", "size is capped at the 2000 maximum");
    }

    #[tokio::test]
    async fn rejects_non_numeric_page_as_400_problem() {
        let (status, ct, _body) = get_line("/?page=abc").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(ct.as_deref(), Some(PROBLEM_CONTENT_TYPE));
    }

    #[tokio::test]
    async fn rejects_zero_page_as_400_problem() {
        let (status, _ct, _body) = get_line("/?page=0").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn rejects_bad_sort_direction_as_400_problem() {
        let (status, _ct, _body) = get_line("/?sort=owner,sideways").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
}
