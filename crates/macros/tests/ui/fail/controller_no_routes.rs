// `#[rest_controller]` with no verb-mapped methods must be a compile error.

use firefly::prelude::*;

struct Empty;

#[rest_controller(path = "/x")]
impl Empty {
    async fn not_a_route(&self) {}
}

fn main() {}
