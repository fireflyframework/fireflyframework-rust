// A return type that is neither `Result` nor `Mono`/`Flux` must be rejected with
// a pointer at the supported shapes.

use firefly::prelude::*;

#[http_client]
trait Api {
    #[get("/x")]
    async fn get(&self) -> String;
}

fn main() {}
