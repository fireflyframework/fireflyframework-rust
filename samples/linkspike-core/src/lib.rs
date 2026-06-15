//! Spike crate: a `@Service` bean + a `#[rest_controller]` controller in a
//! SEPARATE crate from the binary, to prove cross-crate inventory linking via
//! `firefly::link!`.

use std::sync::Arc;

use axum::extract::State;
use firefly::prelude::*;
use firefly::web::WebResult;

/// A plain `@Component`-style bean registered by this (non-bin) crate.
#[derive(Component, Default)]
pub struct SpikeBean;

impl SpikeBean {
    /// A trivial collaborator method the controller calls.
    pub fn hello(&self) -> &'static str {
        "hello-from-core-crate"
    }
}

/// A `@Controller` whose collaborator is autowired from the container.
#[derive(Clone, Controller)]
pub struct SpikeController {
    /// Autowired from the container (registered in this same crate).
    #[autowired]
    pub bean: Arc<SpikeBean>,
}

#[firefly::rest_controller(path = "/spike")]
impl SpikeController {
    /// `GET /spike/ping` — proves the cross-crate controller mounts and its
    /// cross-crate state bean resolves.
    #[get("/ping")]
    async fn ping(State(api): State<SpikeController>) -> WebResult<String> {
        Ok(api.bean.hello().to_string())
    }
}
