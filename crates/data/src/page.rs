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

//! The canonical paged-result envelope.

use serde::{Deserialize, Serialize};

/// Page is the canonical paged-result envelope returned by every Firefly
/// list endpoint. Wire-compatible with the Java/.NET/Go ports' `Page<T>`:
/// it serializes to `{"content": …, "number": …, "size": …,
/// "totalElements": …, "totalPages": …}` so SDK clients dispatch on the
/// same JSON regardless of which port produced it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Page<T> {
    /// The rows of this page.
    pub content: Vec<T>,
    /// Zero-based page index.
    pub number: usize,
    /// Requested page size.
    pub size: usize,
    /// Total elements across all pages.
    pub total_elements: u64,
    /// Total number of pages (`ceil(total_elements / size)`).
    pub total_pages: usize,
}

impl<T> Page<T> {
    /// Constructs a `Page` from raw rows plus paging metadata. Computes
    /// `total_pages` from `total / size`, rounding up; a non-positive
    /// `size` is clamped to 1 first.
    pub fn new(content: Vec<T>, number: usize, size: usize, total: u64) -> Self {
        let size = size.max(1);
        let total_pages = (total as usize).div_ceil(size);
        Page {
            content,
            number,
            size,
            total_elements: total,
            total_pages,
        }
    }

    /// Returns an empty `Page<T>` — no content, all counters zero.
    pub fn empty() -> Self {
        Page {
            content: Vec::new(),
            number: 0,
            size: 0,
            total_elements: 0,
            total_pages: 0,
        }
    }
}

impl<T> Default for Page<T> {
    fn default() -> Self {
        Page::empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Port of Go `TestPage`.
    #[test]
    fn test_page() {
        let p = Page::new(vec![1, 2, 3], 0, 3, 10);
        assert_eq!(p.total_pages, 4, "page: {p:?}");
        assert_eq!(p.number, 0, "page: {p:?}");
        assert_eq!(p.size, 3, "page: {p:?}");
        assert_eq!(p.total_elements, 10, "page: {p:?}");
        assert_eq!(Page::<i32>::empty().total_elements, 0, "empty");
    }

    #[test]
    fn test_page_exact_division() {
        let p = Page::new(vec![1, 2, 3], 0, 3, 9);
        assert_eq!(p.total_pages, 3);
    }

    #[test]
    fn test_page_zero_size_clamped_to_one() {
        let p = Page::<i32>::new(Vec::new(), 0, 0, 5);
        assert_eq!(p.size, 1);
        assert_eq!(p.total_pages, 5);
    }

    /// Wire shape must match the Go port's JSON tags exactly.
    #[test]
    fn test_page_serde_wire_shape() {
        let p = Page::new(vec![1, 2, 3], 0, 3, 10);
        let json = serde_json::to_value(&p).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "content": [1, 2, 3],
                "number": 0,
                "size": 3,
                "totalElements": 10,
                "totalPages": 4,
            })
        );
    }

    #[test]
    fn test_page_serde_round_trip() {
        let p = Page::new(vec!["a".to_string(), "b".to_string()], 1, 2, 4);
        let s = serde_json::to_string(&p).unwrap();
        let back: Page<String> = serde_json::from_str(&s).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn test_empty_page_wire_shape() {
        let json = serde_json::to_string(&Page::<i32>::empty()).unwrap();
        assert_eq!(
            json,
            r#"{"content":[],"number":0,"size":0,"totalElements":0,"totalPages":0}"#
        );
    }

    #[test]
    fn test_default_is_empty() {
        assert_eq!(Page::<i32>::default(), Page::<i32>::empty());
    }
}
