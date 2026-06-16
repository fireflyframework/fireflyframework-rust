// A `:name` path variable with no matching argument must be a compile error.

use firefly::prelude::*;

#[http_client]
trait Api {
    #[get("/:id")]
    async fn get(&self, other: String) -> Result<(), ClientError>;
}

fn main() {}
