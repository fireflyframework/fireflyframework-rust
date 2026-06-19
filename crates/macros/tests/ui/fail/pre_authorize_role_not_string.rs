// A malformed keyword rule (`role` with a non-string value) must fail closed
// with a clear diagnostic rather than being reinterpreted as an expression.

use firefly::security::SecurityError;

struct E;
impl From<SecurityError> for E {
    fn from(_: SecurityError) -> Self {
        E
    }
}

#[firefly::pre_authorize(role = 42)]
async fn f() -> Result<(), E> {
    Ok(())
}

fn main() {}
