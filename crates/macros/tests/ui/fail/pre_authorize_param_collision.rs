// #[pre_authorize] must reject a parameter named `auth`: it would silently
// shadow the framework-injected principal binding, so the rule would read the
// principal instead of the argument.

use firefly::security::SecurityError;

struct E;
impl From<SecurityError> for E {
    fn from(_: SecurityError) -> Self {
        E
    }
}

#[firefly::pre_authorize(auth.principal == owner)]
async fn f(auth: &str, owner: &str) -> Result<(), E> {
    let _ = (auth, owner);
    Ok(())
}

fn main() {}
