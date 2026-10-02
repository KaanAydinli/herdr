use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GitHistoryParams {
    pub cwd: String,
    #[serde(default)]
    pub revision: Option<String>,
    #[serde(default)]
    pub known_head: Option<String>,
    #[serde(default)]
    pub skip: usize,
    #[serde(default = "history_limit")]
    pub limit: u16,
}

fn history_limit() -> u16 {
    100
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GitCommit {
    pub id: String,
    pub parents: Vec<String>,
    pub subject: String,
    pub additions: u64,
    pub deletions: u64,
    pub binary_files: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GitHistory {
    pub head: Option<String>,
    pub commits: Vec<GitCommit>,
    pub has_more: bool,
    pub unchanged: bool,
}
