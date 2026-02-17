use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::util::{append_line, write_string};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RunMeta {
    pub status: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub duration_seconds: Option<i64>,
    pub profile: String,
    pub profile_description: String,
    pub binary: String,
    pub project_dir: String,
    pub host: String,
    pub port: String,
    pub path: String,
    pub keys: String,
    pub seed_demo: u8,
    pub seed_required: u8,
    pub seed_exit_code: Option<i32>,
    pub snapshot_required: u8,
    pub snapshot_status: Option<String>,
    pub snapshot_exit_code: Option<i32>,
    pub vhs_exit_code: Option<i32>,
    pub video_exists: Option<bool>,
    pub snapshot_exists: Option<bool>,
    pub video_duration_seconds: Option<f64>,
    pub output: String,
    pub snapshot: String,
    pub run_dir: String,
    pub trace_id: Option<String>,
    pub fallback_active: Option<bool>,
    pub fallback_reason: Option<String>,
    pub policy_id: Option<String>,
    pub evidence_ledger: Option<String>,
    pub fastapi_output_mode: Option<String>,
    pub fastapi_agent_mode: Option<bool>,
    pub sqlmodel_output_mode: Option<String>,
    pub sqlmodel_agent_mode: Option<bool>,
}

impl RunMeta {
    pub fn write_to_path(&self, path: &Path) -> Result<()> {
        let content = serde_json::to_string_pretty(self)?;
        write_string(path, &content)
    }

    pub fn from_path(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str::<Self>(&content)?)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionRecord {
    pub timestamp: String,
    pub trace_id: String,
    pub decision_id: String,
    pub action: String,
    pub evidence_terms: Vec<String>,
    pub fallback_active: bool,
    pub fallback_reason: Option<String>,
    pub policy_id: String,
}

impl DecisionRecord {
    pub fn append_jsonl(&self, path: &Path) -> Result<()> {
        let line = serde_json::to_string(self)?;
        append_line(path, &line)
    }
}
