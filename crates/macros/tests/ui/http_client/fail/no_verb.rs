// A method with no verb attribute must be a compile error.

use firefly::prelude::*;

#[http_client]
trait Api {
    async fn get(&self) -> Result<(), ClientError>;
}

fn main() {}
