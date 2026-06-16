// The Spring `{id}` path-variable spelling must be rejected with a pointer at
// the axum-style `:id` convention.

use firefly::prelude::*;

#[http_client]
trait Api {
    #[get("/{id}")]
    async fn get(&self, id: String) -> Result<(), ClientError>;
}

fn main() {}
