// A command handler must take exactly one (message) argument.

use firefly::prelude::*;

#[derive(Clone, serde::Serialize, Command)]
struct Cmd {
    x: u32,
}

#[command_handler]
async fn bad(_a: Cmd, _b: u32) -> Result<(), CqrsError> {
    Ok(())
}

fn main() {}
