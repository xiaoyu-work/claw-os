use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct FollowUp {
    pub id: String,
    pub prompt: String,
    pub status: String,
    pub after_task_id: String,
    pub created_at: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct FollowUpQueue {
    pub task_id: String,
    pub session_id: String,
    pub tasks: Vec<FollowUp>,
    pub truncated: bool,
}
