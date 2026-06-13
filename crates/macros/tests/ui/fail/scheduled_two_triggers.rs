// `#[scheduled]` with two triggers must be a compile error.

use firefly::prelude::*;

#[scheduled(cron = "0 0 * * *", fixed_rate = "30s")]
async fn two_triggers() -> Result<(), std::io::Error> {
    Ok(())
}

fn main() {}
