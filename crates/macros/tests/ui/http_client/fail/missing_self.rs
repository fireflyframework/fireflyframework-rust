// A method without a `&self` receiver must be a compile error.

use firefly::prelude::*;

#[http_client]
trait Api {
    #[get("/x")]
    async fn get(id: String) -> Result<(), ClientError>;
}

fn main() {}
