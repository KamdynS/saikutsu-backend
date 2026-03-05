use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct User {
    pub id: Uuid,
    pub email: String,
    #[serde(skip_serializing)]
    pub password_hash: Option<String>,
    pub tier: String,
    pub stripe_customer_id: Option<String>,
    pub daily_reviews_used: i32,
    pub daily_reviews_reset_at: DateTime<Utc>,
    pub monthly_uploads_used: i32,
    pub monthly_exports_used: i32,
    pub monthly_reset_at: DateTime<Utc>,
    pub email_verified: bool,
    pub email_verified_at: Option<DateTime<Utc>>,
    pub is_beta_tester: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct UserResponse {
    pub id: Uuid,
    pub email: String,
    pub tier: String,
    pub email_verified: bool,
    pub is_beta_tester: bool,
    pub daily_reviews_used: i32,
    pub monthly_uploads_used: i32,
    pub monthly_exports_used: i32,
    pub created_at: DateTime<Utc>,
}

impl From<User> for UserResponse {
    fn from(user: User) -> Self {
        Self {
            id: user.id,
            email: user.email,
            tier: user.tier,
            email_verified: user.email_verified,
            is_beta_tester: user.is_beta_tester,
            daily_reviews_used: user.daily_reviews_used,
            monthly_uploads_used: user.monthly_uploads_used,
            monthly_exports_used: user.monthly_exports_used,
            created_at: user.created_at,
        }
    }
}
