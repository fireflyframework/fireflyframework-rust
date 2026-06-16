// A `Result`-returning method must be `async fn` (it is awaited).

use firefly::prelude::*;

#[http_client]
trait Api {
    #[get("/x")]
    fn get(&self) -> Result<(), ClientError>;
}

fn main() {}
