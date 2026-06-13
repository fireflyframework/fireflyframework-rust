// `#[autowired]` requires an `Arc<T>` / `Vec<Arc<T>>` / `Option<Arc<T>>` /
// `Provider<T>` field — a bare type is a compile error with a pointed message.

use firefly::prelude::*;

#[derive(Service)]
struct Bad {
    #[autowired]
    dep: String,
}

fn main() {}
