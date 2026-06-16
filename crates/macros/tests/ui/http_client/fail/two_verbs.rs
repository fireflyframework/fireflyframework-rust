// Two verb attributes on one method must be a compile error.

use firefly::prelude::*;

#[http_client]
trait Api {
    #[get("/a")]
    #[post("/b")]
    async fn both(&self) -> Result<(), ClientError>;
}

fn main() {}
