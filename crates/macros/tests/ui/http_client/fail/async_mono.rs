// A `Mono`-returning method must NOT be `async fn` (the Mono is already
// deferred).

use firefly::prelude::*;
use serde::Deserialize;

#[derive(Deserialize)]
struct Order;

#[http_client]
trait Api {
    #[get("/x")]
    async fn get(&self) -> Mono<Order>;
}

fn main() {}
