// An `Option<_>` (or `Vec<_>` / slice) bound as a `:name` path variable would
// render a garbage URI (e.g. `…/Some(x)` or `…/None`), so it is rejected at
// macro-expansion time with a clean diagnostic.

use firefly::prelude::*;

#[http_client]
trait Api {
    #[get("/:id")]
    async fn get(&self, id: Option<String>) -> Result<(), ClientError>;
}

fn main() {}
