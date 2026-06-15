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

//! `ApplicationContext` end-to-end: builds a shared container, scans the crate
//! graph (honoring profile/conditional gating + eager `#[post_construct]`),
//! resolves beans, and runs `#[pre_destroy]` on close.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use firefly::prelude::*;

static STARTS: AtomicUsize = AtomicUsize::new(0);
static STOPS: AtomicUsize = AtomicUsize::new(0);

#[derive(Repository, Default)]
struct CtxRepo;

#[derive(Service)]
#[firefly(name = "ctx_service", post_construct = "warm", pre_destroy = "cool")]
struct CtxService {
    #[autowired]
    repo: Arc<CtxRepo>,
}

impl CtxService {
    fn warm(&mut self) {
        STARTS.fetch_add(1, Ordering::SeqCst);
    }
    fn cool(&self) {
        STOPS.fetch_add(1, Ordering::SeqCst);
    }
    fn ready(&self) -> bool {
        Arc::strong_count(&self.repo) >= 1
    }
}

// Excluded under the "dev" profile.
#[derive(Service, Default)]
#[firefly(profile = "prod")]
struct ProdOnly;

#[test]
fn application_context_scans_resolves_and_closes() {
    STARTS.store(0, Ordering::SeqCst);
    STOPS.store(0, Ordering::SeqCst);

    let ctx = ApplicationContext::builder().profiles(["dev"]).build();

    // The named service is discovered, wired, and eagerly warmed at startup.
    let svc = ctx
        .container()
        .resolve_named::<CtxService>("ctx_service")
        .expect("named service resolves");
    assert!(svc.ready());
    assert!(
        STARTS.load(Ordering::SeqCst) >= 1,
        "eager #[post_construct] ran at build"
    );

    // Profile-gated bean is excluded under "dev".
    assert!(
        ctx.container().resolve::<ProdOnly>().is_err(),
        "prod-only bean excluded under the dev profile"
    );

    // Introspection surfaces the registered beans.
    assert!(ctx.bean_count() >= 2);
    let beans = ctx.beans();
    assert!(beans.iter().any(|b| b.name == "ctx_service"));

    // close() runs #[pre_destroy].
    ctx.close();
    assert_eq!(STOPS.load(Ordering::SeqCst), 1, "pre_destroy ran on close");
}

#[test]
fn scan_free_function_matches_container_scan() {
    let c = Container::new();
    let n = firefly::scan(&c);
    // The same beans the ApplicationContext discovers are registered here too.
    assert!(n >= 2);
    assert!(c.resolve::<CtxRepo>().is_ok());
}

// An async bean, gated behind the "async-ctx" profile so it is invisible to the
// scans above (which run under "dev"/"default") and only exercised by the two
// async-context tests below.
struct AsyncResource {
    ready: bool,
}

#[derive(Configuration, Default)]
#[firefly(profile = "async-ctx")]
struct AsyncCtxConfig;

#[firefly::bean]
impl AsyncCtxConfig {
    #[bean(profile = "async-ctx")]
    async fn async_resource(&self) -> AsyncResource {
        tokio::task::yield_now().await;
        AsyncResource { ready: true }
    }
}

#[tokio::test]
async fn build_async_awaits_async_beans() {
    let ctx = ApplicationContext::builder()
        .profiles(["async-ctx"])
        .build_async()
        .await
        .expect("build_async");
    let resource = ctx
        .container()
        .resolve::<AsyncResource>()
        .expect("async bean initialized by build_async");
    assert!(resource.ready);

    // The synchronous build does NOT await async beans (the documented
    // limitation); resolving the un-initialized async bean fails discoverably.
    let sync = ApplicationContext::builder()
        .profiles(["async-ctx"])
        .build();
    assert!(
        sync.container().resolve::<AsyncResource>().is_err(),
        "sync build() leaves an async bean uninitialized"
    );
}
