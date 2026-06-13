// `#[bean]` on an impl block with no `#[bean]`-marked methods is a compile
// error — there is nothing to register.

use firefly::prelude::*;

#[derive(Configuration, Default)]
struct EmptyCfg;

#[firefly::bean]
impl EmptyCfg {
    fn helper(&self) -> u32 {
        1
    }
}

fn main() {}
