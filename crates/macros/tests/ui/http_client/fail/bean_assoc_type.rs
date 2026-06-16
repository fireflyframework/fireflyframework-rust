// An associated type makes the trait not `dyn`-compatible, so the `bean` flag
// (which binds `dyn Trait`) rejects it up front rather than failing downstream
// with a cryptic `dyn Trait` error.

use firefly::prelude::*;

#[http_client(bean)]
trait Api {
    type Output;

    #[get("/x")]
    async fn get(&self) -> Result<(), ClientError>;
}

fn main() {}
