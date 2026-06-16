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

//! The [`Multipart`] form / file-upload extractor — Spring MVC's
//! `@RequestParam MultipartFile` / `MultipartHttpServletRequest`, drained into a
//! ready-to-use form.
//!
//! Where axum's [`axum::extract::Multipart`] hands back a streaming iterator the
//! handler must drive (and whose errors escape the framework's problem surface),
//! this extractor **drains the whole form up front** into named text
//! [`fields`](Multipart::text) and uploaded [`files`](Multipart::files), turning
//! any decode failure into an RFC 9457 `application/problem+json` **400** before
//! the handler runs. A part with a `filename` is a [`UploadedFile`]; every other
//! part is a text field.

use std::collections::HashMap;

use axum::body::Bytes;
use axum::extract::{FromRequest, Multipart as AxumMultipart, Request};
use axum::response::{IntoResponse, Response};
use firefly_kernel::FireflyError;

use crate::problem::WebError;

/// One uploaded file part of a [`Multipart`] form — the Rust analog of a Spring
/// `MultipartFile`.
#[derive(Debug, Clone)]
pub struct UploadedFile {
    /// The form field name the file was submitted under.
    pub field_name: String,
    /// The client-supplied original file name, if any (`filename="…"`).
    pub file_name: Option<String>,
    /// The part's declared `Content-Type`, if any.
    pub content_type: Option<String>,
    /// The file's raw bytes (drained into memory).
    pub bytes: Bytes,
}

impl UploadedFile {
    /// The file size in bytes.
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Whether the uploaded file is empty.
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// The file content as UTF-8 text, or `None` if it is not valid UTF-8.
    pub fn text(&self) -> Option<&str> {
        std::str::from_utf8(&self.bytes).ok()
    }
}

/// A fully-drained `multipart/form-data` request: named text fields plus the
/// uploaded files. Extract it on a handler exactly like any other argument.
///
/// ```ignore
/// use firefly::web::Multipart;
///
/// async fn upload(form: Multipart) -> WebResult<String> {
///     let title = form.text("title").unwrap_or("untitled");
///     let avatar = form.file("avatar").ok_or_else(|| /* 400 */ )?;
///     store(avatar.file_name.as_deref(), &avatar.bytes).await?;
///     Ok(format!("{title}: {} bytes", avatar.len()))
/// }
/// ```
#[derive(Debug, Clone, Default)]
pub struct Multipart {
    fields: HashMap<String, String>,
    files: Vec<UploadedFile>,
}

impl Multipart {
    /// The value of a text field by name (the **last** if repeated).
    pub fn text(&self, name: &str) -> Option<&str> {
        self.fields.get(name).map(String::as_str)
    }

    /// The first uploaded file submitted under `name`.
    pub fn file(&self, name: &str) -> Option<&UploadedFile> {
        self.files.iter().find(|f| f.field_name == name)
    }

    /// Every uploaded file, in submission order.
    pub fn files(&self) -> &[UploadedFile] {
        &self.files
    }

    /// Consumes the form, returning the uploaded files.
    pub fn into_files(self) -> Vec<UploadedFile> {
        self.files
    }

    /// Whether the form carried no fields and no files.
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty() && self.files.is_empty()
    }
}

#[axum::async_trait]
impl<S> FromRequest<S> for Multipart
where
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let mut multipart = AxumMultipart::from_request(req, state)
            .await
            .map_err(|rejection| problem(rejection.body_text()))?;

        let mut form = Multipart::default();
        while let Some(field) = multipart
            .next_field()
            .await
            .map_err(|e| problem(e.body_text()))?
        {
            // The borrowing metadata must be captured before `bytes()`/`text()`
            // consume the field.
            let field_name = field.name().unwrap_or_default().to_owned();
            let file_name = field.file_name().map(str::to_owned);
            let content_type = field.content_type().map(str::to_owned);

            if file_name.is_some() {
                let bytes = field.bytes().await.map_err(|e| problem(e.body_text()))?;
                form.files.push(UploadedFile {
                    field_name,
                    file_name,
                    content_type,
                    bytes,
                });
            } else {
                let value = field.text().await.map_err(|e| problem(e.body_text()))?;
                form.fields.insert(field_name, value);
            }
        }
        Ok(form)
    }
}

/// Renders a multipart decode failure as a 400 RFC 9457 problem.
fn problem(detail: String) -> Response {
    WebError::from(FireflyError::bad_request(detail)).into_response()
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::routing::post;
    use axum::Router;
    use firefly_kernel::PROBLEM_CONTENT_TYPE;
    use http::{header, Request, StatusCode};
    use tower::ServiceExt;

    use super::Multipart;

    async fn handler(form: Multipart) -> String {
        let title = form.text("title").unwrap_or("∅").to_owned();
        match form.file("doc") {
            Some(f) => format!(
                "{title}|{}|{}|{}",
                f.file_name.as_deref().unwrap_or("?"),
                f.content_type.as_deref().unwrap_or("?"),
                f.text().unwrap_or("?")
            ),
            None => format!("{title}|no-file"),
        }
    }

    async fn post_multipart(
        content_type: &str,
        body: &str,
    ) -> (StatusCode, Option<String>, String) {
        let res = Router::new()
            .route("/upload", post(handler))
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/upload")
                    .header(header::CONTENT_TYPE, content_type)
                    .body(Body::from(body.to_owned()))
                    .unwrap(),
            )
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
    async fn drains_text_fields_and_files() {
        let boundary = "FFBOUNDARY";
        let body = format!(
            "--{b}\r\n\
             Content-Disposition: form-data; name=\"title\"\r\n\r\n\
             Quarterly Report\r\n\
             --{b}\r\n\
             Content-Disposition: form-data; name=\"doc\"; filename=\"q3.txt\"\r\n\
             Content-Type: text/plain\r\n\r\n\
             ledger lines\r\n\
             --{b}--\r\n",
            b = boundary
        );
        let (status, _ct, out) =
            post_multipart(&format!("multipart/form-data; boundary={boundary}"), &body).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(out, "Quarterly Report|q3.txt|text/plain|ledger lines");
    }

    #[tokio::test]
    async fn text_only_form_has_no_file() {
        let boundary = "FFBOUNDARY";
        let body = format!(
            "--{b}\r\n\
             Content-Disposition: form-data; name=\"title\"\r\n\r\n\
             solo\r\n\
             --{b}--\r\n",
            b = boundary
        );
        let (status, _ct, out) =
            post_multipart(&format!("multipart/form-data; boundary={boundary}"), &body).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(out, "solo|no-file");
    }

    #[tokio::test]
    async fn malformed_multipart_rejects_with_problem() {
        // A `multipart/form-data` content-type with no boundary is undecodable.
        let (status, ct, _out) = post_multipart("multipart/form-data", "garbage").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(ct.as_deref(), Some(PROBLEM_CONTENT_TYPE));
    }
}
