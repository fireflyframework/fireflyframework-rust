// Two body-eligible (non-scalar) arguments with no `#[body]` are ambiguous.

use firefly::prelude::*;
use serde::Serialize;

#[derive(Serialize)]
struct A;
#[derive(Serialize)]
struct B;

#[http_client]
trait Api {
    #[post("/x")]
    async fn create(&self, a: A, b: B) -> Result<(), ClientError>;
}

fn main() {}
