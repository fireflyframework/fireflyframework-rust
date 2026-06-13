// `#[scheduled]` with no trigger must be a compile error (pyfly's runtime
// ValueError, lifted to compile time).

use firefly::prelude::*;

#[scheduled]
async fn no_trigger() -> Result<(), std::io::Error> {
    Ok(())
}

fn main() {}
