// A generic method under `bean` cannot satisfy the `dyn Trait` autowire bind, so
// the macro rejects it up front.

use firefly::prelude::*;
use serde::Serialize;

#[http_client(bean)]
trait Api {
    #[post("/x")]
    async fn create<T: Serialize>(&self, #[body] body: T) -> Result<(), ClientError>;
}

fn main() {}
