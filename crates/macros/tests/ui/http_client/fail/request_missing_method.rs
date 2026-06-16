// `#[request(...)]` requires an explicit `method = "..."`; omitting it is a clean
// compile error rather than a defaulted verb.

use firefly::prelude::*;

#[http_client]
trait Api {
    #[request(path = "/x")]
    async fn call(&self) -> Result<(), ClientError>;
}

fn main() {}
