use serde::{Deserialize, Serialize};
use tracing::info;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LoginMethod {
    Password(String),
    EmailOtp(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthSession {
    pub username: String,
    pub auth_token: Option<String>,
    pub refresh_token: Option<String>,
}

pub struct AuthFlow;

impl AuthFlow {
    pub async fn query_auth_method(identifier: &str) -> Result<bool, String> {
        info!("Checking authentication method for {}", identifier);
        // Returns true if Spotify asks for OTP email code, false if password prompt
        Ok(false)
    }

    pub async fn authenticate(identifier: &str, method: LoginMethod) -> Result<AuthSession, String> {
        info!("Authenticating {} with method {:?}", identifier, method);
        Ok(AuthSession {
            username: identifier.to_string(),
            auth_token: Some("mock_token".into()),
            refresh_token: Some("mock_refresh".into()),
        })
    }
}
