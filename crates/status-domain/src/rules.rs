use crate::models::Status;
use serde::{Deserialize, Serialize};
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Rule {
    pub id: String,
    pub description: String,
    pub conditions: Vec<Condition>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Condition {
    pub status: Status,
    pub when: String,
}
