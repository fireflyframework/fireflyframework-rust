//! OAuth2 stack — client registrations, browser login flow,
//! authorization server, and pluggable token stores (pyfly:
//! `pyfly.security.oauth2` + `pyfly.security.adapters`).
//!
//! - [`ClientRegistration`] + [`google`]/[`github`]/[`keycloak`]
//!   presets and the [`ClientRegistrationRepository`] port.
//! - [`OAuth2LoginHandler`] — axum router for the authorization_code
//!   flow with `state`/`nonce` and PKCE S256.
//! - [`AuthorizationServer`] — `client_credentials` + `refresh_token`
//!   grants issuing HS256 JWTs, persisting refresh tokens through the
//!   [`TokenStore`] port ([`InMemoryTokenStore`], [`RedisTokenStore`],
//!   [`PostgresTokenStore`]).
//!
//! The resource-server side (JWKS validation) lives at the crate root
//! as [`JwksVerifier`](crate::JwksVerifier), since it implements the
//! crate-wide [`Verifier`](crate::Verifier) port.

mod authorization_server;
mod client;
mod login;
mod token_store;

pub use authorization_server::{AuthorizationServer, OAuth2Error, TokenRequest, TokenResponse};
pub use client::{
    github, google, keycloak, ClientRegistration, ClientRegistrationRepository,
    InMemoryClientRegistrationRepository,
};
pub use login::{
    generate_pkce, pkce_challenge, FixedLoginSessionStore, InMemoryLoginSession, LoginSession,
    LoginSessionStore, OAuth2LoginHandler, SESSION_KEY_NONCE, SESSION_KEY_PKCE_VERIFIER,
    SESSION_KEY_REDIRECT_URI, SESSION_KEY_SECURITY_CONTEXT, SESSION_KEY_STATE,
};
pub use token_store::{
    validate_table_name, InMemoryTokenStore, PostgresTokenStore, RedisTokenStore, TokenStore,
    POSTGRES_TOKEN_TABLE, REDIS_TOKEN_KEY_PREFIX,
};
