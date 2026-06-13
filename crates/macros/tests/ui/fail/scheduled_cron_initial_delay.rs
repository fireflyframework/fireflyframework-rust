// `#[scheduled(cron = ..., initial_delay = ...)]` must be a compile error:
// `initial_delay` has no meaning for a cron trigger (the cron branch never
// consumes it), so silently ignoring it would mislead a user expecting a
// delayed first firing. Mirrors the existing `zone`-requires-`cron` check.

use firefly::prelude::*;

#[scheduled(cron = "0 0 * * *", initial_delay = "5s")]
async fn cron_with_initial_delay() -> Result<(), std::io::Error> {
    Ok(())
}

fn main() {}
