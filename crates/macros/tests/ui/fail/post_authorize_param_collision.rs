// #[post_authorize] must reject a parameter named `result`: it would silently
// shadow the framework-injected return-value binding, authorizing against the
// wrong value (a fail-open trap).

use firefly::security::SecurityError;

struct E;
impl From<SecurityError> for E {
    fn from(_: SecurityError) -> Self {
        E
    }
}

#[firefly::post_authorize(result == 1)]
async fn f(result: i32) -> Result<i32, E> {
    Ok(result)
}

fn main() {}
