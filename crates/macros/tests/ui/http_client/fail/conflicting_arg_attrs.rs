// Two binding attributes on one argument (`#[path]` + `#[query]`) conflict.

use firefly::prelude::*;

#[http_client]
trait Api {
    #[get("/:id")]
    async fn get(&self, #[path] #[query] id: String) -> Result<(), ClientError>;
}

fn main() {}
