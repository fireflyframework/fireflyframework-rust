// Two `#[body]` arguments must be a compile error.

use firefly::prelude::*;
use serde::Serialize;

#[derive(Serialize)]
struct A;
#[derive(Serialize)]
struct B;

#[http_client]
trait Api {
    #[post("/x")]
    async fn create(&self, #[body] a: A, #[body] b: B) -> Result<(), ClientError>;
}

fn main() {}
