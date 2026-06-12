//! Mirrors the README quick-start snippet so the documented example is
//! guaranteed to compile and behave as written.

use firefly_validators::{
    validate_email, validate_iban, validate_password, PasswordPolicy, ValidationError,
};

struct RegisterCmd {
    email: String,
    password: String,
    iban: String,
}

impl RegisterCmd {
    fn validate(&self) -> Result<(), Vec<ValidationError>> {
        let checks = [
            validate_email(&self.email),
            validate_password(&self.password, PasswordPolicy::default()),
            validate_iban(&self.iban),
        ];
        let errs: Vec<ValidationError> = checks.into_iter().filter_map(Result::err).collect();
        if errs.is_empty() {
            Ok(())
        } else {
            Err(errs)
        }
    }
}

#[test]
fn readme_quickstart() {
    let cmd = RegisterCmd {
        email: "alice@example.com".into(),
        password: "Hello-World-1!".into(),
        iban: "ES91 2100 0418 4502 0005 1332".into(),
    };
    assert!(cmd.validate().is_ok());

    let err = validate_iban("GB82WEST12345698765431").unwrap_err();
    assert_eq!(err.reason(), "iban: mod-97 mismatch");
}

#[test]
fn multi_field_failure_collects_every_reason() {
    let cmd = RegisterCmd {
        email: "not-an-email".into(),
        password: "weak".into(),
        iban: "XX00".into(),
    };
    let errs = cmd.validate().expect_err("all three fields invalid");
    assert_eq!(errs.len(), 3);
    assert!(errs[0].reason().starts_with("email:"));
    assert!(errs[1].reason().starts_with("password:"));
    assert!(errs[2].reason().starts_with("iban:"));
}
