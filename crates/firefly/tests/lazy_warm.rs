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

//! `#[firefly(lazy)]` (Spring `@Lazy`): a lazy singleton opts out of the
//! `ApplicationContext` eager warm-up pass — its `#[post_construct]` must NOT
//! fire at build, only on first resolve.

use std::sync::atomic::{AtomicUsize, Ordering};

use firefly::prelude::*;

static LAZY_WARMED: AtomicUsize = AtomicUsize::new(0);

#[derive(Service)]
#[firefly(name = "lazy_svc", lazy, post_construct = "warm")]
struct LazySvc;

impl LazySvc {
    fn warm(&mut self) {
        LAZY_WARMED.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn lazy_bean_skips_eager_warmup_then_builds_on_resolve() {
    LAZY_WARMED.store(0, Ordering::SeqCst);

    // Eager-by-default context build warms every NON-lazy singleton.
    let ctx = ApplicationContext::builder().build();
    assert_eq!(
        LAZY_WARMED.load(Ordering::SeqCst),
        0,
        "a #[firefly(lazy)] bean must not be eagerly warmed at startup"
    );

    // Resolving it builds it on demand, so #[post_construct] fires now.
    let _svc = ctx
        .container()
        .resolve_named::<LazySvc>("lazy_svc")
        .expect("lazy bean resolves on demand");
    assert_eq!(
        LAZY_WARMED.load(Ordering::SeqCst),
        1,
        "the lazy bean is built (post_construct runs) on first resolve"
    );
}
