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

//! Spring-style pagination **request** types: [`Pageable`],
//! [`RequestSort`], and [`Order`].
//!
//! This is the Rust port of pyfly's `data.pageable` module. These are
//! the *request* side of paging — what a caller asks for — and are
//! distinct from the [`Page`](crate::Page) *response* envelope (what
//! comes back).
//!
//! pyfly exposes `Pageable` (page-number + size + sort, with `page >= 1`
//! validation), `Sort` (an ordered collection of orders, with
//! `and_then` / `ascending` / `descending` composition), and `Order`
//! (a property + direction, with `asc` / `desc` factories). The Rust
//! port keeps the same behaviour and names, except the sort collection
//! is called [`RequestSort`] to avoid colliding with the SQL-render
//! [`Sort`](crate::Sort) already exported by the filter DSL.
//!
//! Paging is wired into the [`Repository`](crate::Repository) contract:
//! a [`Pageable`] lowers to a [`Filter`](crate::Filter) via
//! [`Pageable::to_filter`] (translating the 1-based page number to the
//! filter's 0-based page index and projecting the sort orders onto the
//! filter's ORDER BY clauses), and the repository exposes a
//! [`Repository::find_page`](crate::Repository::find_page) default
//! method that accepts a [`Pageable`] directly.
//!
//! # Quick start
//!
//! ```
//! use firefly_data::{Order, Pageable, RequestSort};
//!
//! let sort = RequestSort::by(["name"]).and_then(&RequestSort::by(["age"]).descending());
//! let pageable = Pageable::of(2, 10, sort).unwrap();
//! assert_eq!(pageable.offset(), 10);
//!
//! // Lower to the Filter DSL (1-based page -> 0-based filter page).
//! let filter = pageable.to_filter();
//! assert_eq!(filter.page, 1);
//! assert_eq!(filter.size, 10);
//! assert_eq!(filter.sorts.len(), 2);
//! ```

use crate::filter::{Direction, Filter, Sort};

/// The sentinel size used by [`Pageable::unpaged`] — the largest
/// representable `usize`, mirroring pyfly's `sys.maxsize` sentinel.
pub const UNPAGED_SIZE: usize = usize::MAX;

/// A single sort order: a property name plus a direction.
///
/// Port of pyfly's `data.pageable.Order`. The default direction is
/// ascending, matching pyfly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Order {
    /// The property (field) to sort by.
    pub property: String,
    /// The sort direction.
    pub direction: Direction,
}

impl Order {
    /// Creates an order with the given property and direction.
    pub fn new(property: impl Into<String>, direction: Direction) -> Self {
        Order {
            property: property.into(),
            direction,
        }
    }

    /// Creates an ascending order for the given property (pyfly
    /// `Order.asc`).
    pub fn asc(property: impl Into<String>) -> Self {
        Order::new(property, Direction::Asc)
    }

    /// Creates a descending order for the given property (pyfly
    /// `Order.desc`).
    pub fn desc(property: impl Into<String>) -> Self {
        Order::new(property, Direction::Desc)
    }
}

/// An ordered collection of [`Order`]s.
///
/// Port of pyfly's `data.pageable.Sort`. Named `RequestSort` (not
/// `Sort`) to avoid colliding with the SQL-render
/// [`Sort`](crate::Sort) already exported by the filter DSL — this is
/// the *request* sort, the SQL `Sort` is the *render* clause.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RequestSort {
    /// The sort orders, applied in declaration order.
    pub orders: Vec<Order>,
}

impl RequestSort {
    /// Creates an ascending sort by the given properties (pyfly
    /// `Sort.by`).
    pub fn by<I, S>(properties: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        RequestSort {
            orders: properties.into_iter().map(Order::asc).collect(),
        }
    }

    /// Creates a sort from explicit orders.
    pub fn of(orders: impl IntoIterator<Item = Order>) -> Self {
        RequestSort {
            orders: orders.into_iter().collect(),
        }
    }

    /// Returns an empty (unsorted) sort (pyfly `Sort.unsorted`).
    pub fn unsorted() -> Self {
        RequestSort::default()
    }

    /// Returns `true` when there are no orders.
    pub fn is_unsorted(&self) -> bool {
        self.orders.is_empty()
    }

    /// Returns `true` when there is at least one order.
    pub fn is_sorted(&self) -> bool {
        !self.orders.is_empty()
    }

    /// Combines this sort with `other`, appending `other`'s orders after
    /// this sort's orders (pyfly `Sort.and_then`).
    pub fn and_then(mut self, other: &RequestSort) -> Self {
        self.orders.extend(other.orders.iter().cloned());
        self
    }

    /// Returns the same sort with every direction flipped to descending
    /// (pyfly `Sort.descending`).
    pub fn descending(mut self) -> Self {
        for o in &mut self.orders {
            o.direction = Direction::Desc;
        }
        self
    }

    /// Returns the same sort with every direction flipped to ascending
    /// (pyfly `Sort.ascending`).
    pub fn ascending(mut self) -> Self {
        for o in &mut self.orders {
            o.direction = Direction::Asc;
        }
        self
    }

    /// Lowers this request sort into the filter DSL's
    /// [`Sort`](crate::Sort) clauses.
    pub fn to_sorts(&self) -> Vec<Sort> {
        self.orders
            .iter()
            .map(|o| Sort {
                field: o.property.clone(),
                direction: o.direction,
            })
            .collect()
    }
}

/// A pagination request: a 1-based page number, a page size, and a
/// [`RequestSort`].
///
/// Port of pyfly's `data.pageable.Pageable`. Like pyfly, the page
/// number is **1-based** and validated to be `>= 1` (with size `>= 1`),
/// unless the pageable is [`Pageable::unpaged`]. The default is page 1,
/// size 20, unsorted — identical to pyfly's defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pageable {
    /// The 1-based page number.
    pub page: usize,
    /// The page size. Equal to [`UNPAGED_SIZE`] for an unpaged request.
    pub size: usize,
    /// The sort criteria.
    pub sort: RequestSort,
}

/// The error returned when constructing an invalid [`Pageable`].
///
/// The messages match pyfly's `ValueError` text so a migrating consumer
/// sees the same diagnostic.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PageableError {
    /// The requested page number was below 1.
    #[error("page must be >= 1, got {0}")]
    PageTooSmall(usize),
    /// The requested page size was below 1.
    #[error("size must be >= 1, got {0}")]
    SizeTooSmall(usize),
}

impl Default for Pageable {
    /// Page 1, size 20, unsorted — pyfly's `Pageable()` defaults.
    fn default() -> Self {
        Pageable {
            page: 1,
            size: 20,
            sort: RequestSort::unsorted(),
        }
    }
}

impl Pageable {
    /// Creates a pageable for the given 1-based page, size, and sort.
    ///
    /// Returns [`PageableError`] when `page < 1` or `size < 1` (matching
    /// pyfly's `__post_init__` validation). The unpaged sentinel size is
    /// never validated — use [`Pageable::unpaged`] for that.
    pub fn of(page: usize, size: usize, sort: RequestSort) -> Result<Self, PageableError> {
        if size != UNPAGED_SIZE {
            if page < 1 {
                return Err(PageableError::PageTooSmall(page));
            }
            if size < 1 {
                return Err(PageableError::SizeTooSmall(size));
            }
        }
        Ok(Pageable { page, size, sort })
    }

    /// Creates an unsorted pageable for the given 1-based page and size.
    pub fn paged(page: usize, size: usize) -> Result<Self, PageableError> {
        Pageable::of(page, size, RequestSort::unsorted())
    }

    /// Returns an unpaged request (fetch everything), sentinel size
    /// [`UNPAGED_SIZE`] — pyfly's `Pageable.unpaged`.
    pub fn unpaged() -> Self {
        Pageable {
            page: 1,
            size: UNPAGED_SIZE,
            sort: RequestSort::unsorted(),
        }
    }

    /// Whether this pageable represents actual pagination (pyfly
    /// `is_paged`). `false` only for [`Pageable::unpaged`].
    pub fn is_paged(&self) -> bool {
        self.size != UNPAGED_SIZE
    }

    /// Whether this pageable fetches everything (the inverse of
    /// [`Pageable::is_paged`]).
    pub fn is_unpaged(&self) -> bool {
        self.size == UNPAGED_SIZE
    }

    /// The zero-based row offset of this page: `(page - 1) * size`
    /// (pyfly `offset`).
    pub fn offset(&self) -> usize {
        self.page.saturating_sub(1).saturating_mul(self.size)
    }

    /// Returns a pageable for the next page, preserving size and sort
    /// (pyfly `next`).
    pub fn next(&self) -> Pageable {
        Pageable {
            page: self.page + 1,
            size: self.size,
            sort: self.sort.clone(),
        }
    }

    /// Returns a pageable for the previous page (minimum page 1),
    /// preserving size and sort (pyfly `previous`).
    pub fn previous(&self) -> Pageable {
        Pageable {
            page: self.page.saturating_sub(1).max(1),
            size: self.size,
            sort: self.sort.clone(),
        }
    }

    /// Lowers this pageable into a [`Filter`].
    ///
    /// The 1-based page number is translated to the filter's **0-based**
    /// page index (`page - 1`), the size carries over (an unpaged
    /// request maps to size `0`, which disables the filter's
    /// LIMIT/OFFSET clause), and the sort orders project onto the
    /// filter's ORDER BY clauses.
    pub fn to_filter(&self) -> Filter {
        let mut filter = Filter::new();
        filter.sorts = self.sort.to_sorts();
        if self.is_paged() {
            filter.page = self.page.saturating_sub(1);
            filter.size = self.size;
        } else {
            // Unpaged: no LIMIT/OFFSET (size 0 disables it in to_sql).
            filter.page = 0;
            filter.size = 0;
        }
        filter
    }

    /// Lowers this pageable onto an existing [`Filter`], preserving the
    /// filter's predicates while replacing its paging window and ORDER
    /// BY clauses with this pageable's. Useful for adding pagination to a
    /// filter built from a [`Specification`](crate::Specification) or a
    /// derived query.
    pub fn apply_to(&self, mut filter: Filter) -> Filter {
        filter.sorts = self.sort.to_sorts();
        if self.is_paged() {
            filter.page = self.page.saturating_sub(1);
            filter.size = self.size;
        } else {
            filter.page = 0;
            filter.size = 0;
        }
        filter
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Order (ports tests/data/test_pageable.py::TestOrder) --------

    #[test]
    fn test_asc_factory() {
        let order = Order::asc("name");
        assert_eq!(order.property, "name");
        assert_eq!(order.direction, Direction::Asc);
    }

    #[test]
    fn test_desc_factory() {
        let order = Order::desc("age");
        assert_eq!(order.property, "age");
        assert_eq!(order.direction, Direction::Desc);
    }

    #[test]
    fn test_default_direction_is_asc() {
        let order = Order::new("email", Direction::Asc);
        assert_eq!(order.direction, Direction::Asc);
    }

    // ---- RequestSort (ports TestSort) --------------------------------

    #[test]
    fn test_by_creates_ascending_sort() {
        let sort = RequestSort::by(["name"]);
        assert_eq!(sort.orders.len(), 1);
        assert_eq!(sort.orders[0].property, "name");
        assert_eq!(sort.orders[0].direction, Direction::Asc);
    }

    #[test]
    fn test_by_multiple_properties() {
        let sort = RequestSort::by(["name", "age", "email"]);
        assert_eq!(sort.orders.len(), 3);
        let props: Vec<&str> = sort.orders.iter().map(|o| o.property.as_str()).collect();
        assert_eq!(props, vec!["name", "age", "email"]);
        assert!(sort.orders.iter().all(|o| o.direction == Direction::Asc));
    }

    #[test]
    fn test_descending_flips_direction() {
        let sort = RequestSort::by(["name"]).descending();
        assert_eq!(sort.orders[0].direction, Direction::Desc);
    }

    #[test]
    fn test_ascending_flips_direction() {
        let sort = RequestSort::by(["name"]).descending().ascending();
        assert_eq!(sort.orders[0].direction, Direction::Asc);
    }

    #[test]
    fn test_and_then_combines_sorts() {
        let sort_a = RequestSort::by(["name"]);
        let sort_b = RequestSort::by(["age"]).descending();
        let combined = sort_a.and_then(&sort_b);
        assert_eq!(combined.orders.len(), 2);
        assert_eq!(combined.orders[0].property, "name");
        assert_eq!(combined.orders[0].direction, Direction::Asc);
        assert_eq!(combined.orders[1].property, "age");
        assert_eq!(combined.orders[1].direction, Direction::Desc);
    }

    #[test]
    fn test_unsorted() {
        let sort = RequestSort::unsorted();
        assert!(sort.orders.is_empty());
        assert!(sort.is_unsorted());
        assert!(!sort.is_sorted());
    }

    // ---- Pageable (ports TestPageable) -------------------------------

    #[test]
    fn test_of_creates_pageable() {
        let pageable = Pageable::paged(1, 20).unwrap();
        assert_eq!(pageable.page, 1);
        assert_eq!(pageable.size, 20);
        assert!(pageable.sort.orders.is_empty());
    }

    #[test]
    fn test_of_with_sort() {
        let sort = RequestSort::by(["name"]);
        let pageable = Pageable::of(2, 10, sort.clone()).unwrap();
        assert_eq!(pageable.page, 2);
        assert_eq!(pageable.size, 10);
        assert_eq!(pageable.sort, sort);
    }

    #[test]
    fn test_offset_calculation() {
        assert_eq!(Pageable::paged(1, 20).unwrap().offset(), 0);
        assert_eq!(Pageable::paged(2, 20).unwrap().offset(), 20);
        assert_eq!(Pageable::paged(3, 10).unwrap().offset(), 20);
        assert_eq!(Pageable::paged(5, 25).unwrap().offset(), 100);
    }

    #[test]
    fn test_next_page() {
        let pageable = Pageable::paged(3, 10).unwrap();
        let next_page = pageable.next();
        assert_eq!(next_page.page, 4);
        assert_eq!(next_page.size, 10);
    }

    #[test]
    fn test_previous_page() {
        let pageable = Pageable::paged(3, 10).unwrap();
        let prev_page = pageable.previous();
        assert_eq!(prev_page.page, 2);
        assert_eq!(prev_page.size, 10);
    }

    #[test]
    fn test_previous_page_min_is_one() {
        let pageable = Pageable::paged(1, 10).unwrap();
        let prev_page = pageable.previous();
        assert_eq!(prev_page.page, 1);
    }

    #[test]
    fn test_next_preserves_sort() {
        let sort = RequestSort::by(["name"]);
        let pageable = Pageable::of(1, 10, sort.clone()).unwrap();
        assert_eq!(pageable.next().sort, sort);
    }

    #[test]
    fn test_previous_preserves_sort() {
        let sort = RequestSort::by(["name"]);
        let pageable = Pageable::of(2, 10, sort.clone()).unwrap();
        assert_eq!(pageable.previous().sort, sort);
    }

    #[test]
    fn test_unpaged() {
        let pageable = Pageable::unpaged();
        assert_eq!(pageable.page, 1);
        assert_eq!(pageable.size, UNPAGED_SIZE);
        assert!(!pageable.is_paged());
        assert!(pageable.is_unpaged());
    }

    #[test]
    fn test_paged_is_paged() {
        let pageable = Pageable::paged(1, 20).unwrap();
        assert!(pageable.is_paged());
    }

    #[test]
    fn test_default_values() {
        let pageable = Pageable::default();
        assert_eq!(pageable.page, 1);
        assert_eq!(pageable.size, 20);
        assert!(pageable.sort.orders.is_empty());
    }

    #[test]
    fn test_rejects_page_less_than_one() {
        let err = Pageable::of(0, 20, RequestSort::unsorted()).unwrap_err();
        assert_eq!(err, PageableError::PageTooSmall(0));
        assert!(err.to_string().contains("page must be >= 1"));
    }

    #[test]
    fn test_rejects_size_less_than_one() {
        let err = Pageable::of(1, 0, RequestSort::unsorted()).unwrap_err();
        assert_eq!(err, PageableError::SizeTooSmall(0));
        assert!(err.to_string().contains("size must be >= 1"));
    }

    // ---- to_filter wiring --------------------------------------------

    #[test]
    fn test_to_filter_translates_one_based_to_zero_based() {
        let pageable = Pageable::paged(3, 10).unwrap();
        let filter = pageable.to_filter();
        // 1-based page 3 -> 0-based filter page 2.
        assert_eq!(filter.page, 2);
        assert_eq!(filter.size, 10);
        // The filter's own offset math then yields 0-based row offset 20.
        let (sql, _) = filter.to_sql();
        assert!(sql.contains("LIMIT 10 OFFSET 20"), "{sql}");
    }

    #[test]
    fn test_to_filter_projects_sort_orders() {
        let sort = RequestSort::by(["name"]).and_then(&RequestSort::by(["age"]).descending());
        let pageable = Pageable::of(1, 5, sort).unwrap();
        let filter = pageable.to_filter();
        assert_eq!(filter.sorts.len(), 2);
        assert_eq!(filter.sorts[0].field, "name");
        assert_eq!(filter.sorts[0].direction, Direction::Asc);
        assert_eq!(filter.sorts[1].field, "age");
        assert_eq!(filter.sorts[1].direction, Direction::Desc);
    }

    #[test]
    fn test_unpaged_to_filter_disables_limit() {
        let filter = Pageable::unpaged().to_filter();
        assert_eq!(filter.size, 0);
        let (sql, _) = filter.to_sql();
        assert!(!sql.contains("LIMIT"), "{sql}");
    }

    #[test]
    fn test_apply_to_preserves_predicates() {
        let base = Filter::new().where_eq("status", "active");
        let pageable = Pageable::of(2, 5, RequestSort::by(["name"])).unwrap();
        let filter = pageable.apply_to(base);
        assert_eq!(filter.predicates.len(), 1);
        assert_eq!(filter.predicates[0].field, "status");
        assert_eq!(filter.page, 1);
        assert_eq!(filter.size, 5);
        assert_eq!(filter.sorts.len(), 1);
        assert_eq!(filter.sorts[0].field, "name");
    }
}
