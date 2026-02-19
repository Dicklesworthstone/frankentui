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
    pub host_vhs_exit_code: Option<i32>,
    pub vhs_driver_used: Option<String>,
    pub vhs_docker_log: Option<String>,
    pub video_exists: Option<bool>,
    pub snapshot_exists: Option<bool>,
    pub video_duration_seconds: Option<f64>,
    pub output: String,
    pub snapshot: String,
    pub run_dir: String,
    pub trace_id: Option<String>,
    pub fallback_active: Option<bool>,
    pub fallback_reason: Option<String>,
    pub capture_error_reason: Option<String>,
    pub ttyd_shim_log: Option<String>,
    pub ttyd_runtime_log: Option<String>,
    pub vhs_no_sandbox_forced: Option<bool>,
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

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::{DecisionRecord, RunMeta};

    #[test]
    fn runmeta_round_trip_preserves_fields() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("run_meta.json");

        let original = RunMeta {
            status: "ok".to_string(),
            started_at: "2026-02-17T00:00:00Z".to_string(),
            finished_at: Some("2026-02-17T00:00:01Z".to_string()),
            duration_seconds: Some(1),
            profile: "analytics-empty".to_string(),
            binary: "cargo run -q -p ftui-demo-showcase".to_string(),
            output: "/tmp/out.mp4".to_string(),
            run_dir: "/tmp/run".to_string(),
            fastapi_output_mode: Some("plain".to_string()),
            sqlmodel_output_mode: Some("json".to_string()),
            ..RunMeta::default()
        };

        original.write_to_path(&path).expect("write run_meta");
        let decoded = RunMeta::from_path(&path).expect("read run_meta");

        assert_eq!(decoded.status, original.status);
        assert_eq!(decoded.started_at, original.started_at);
        assert_eq!(decoded.finished_at, original.finished_at);
        assert_eq!(decoded.duration_seconds, original.duration_seconds);
        assert_eq!(decoded.profile, original.profile);
        assert_eq!(decoded.binary, original.binary);
        assert_eq!(decoded.output, original.output);
        assert_eq!(decoded.run_dir, original.run_dir);
        assert_eq!(decoded.fastapi_output_mode, original.fastapi_output_mode);
        assert_eq!(decoded.sqlmodel_output_mode, original.sqlmodel_output_mode);
    }

    #[test]
    fn runmeta_deserialize_sparse_json_uses_defaults_for_missing_fields() {
        let sparse = r#"{"status":"failed","started_at":"2026-02-17T00:00:00Z"}"#;
        let parsed = serde_json::from_str::<RunMeta>(sparse).expect("parse sparse runmeta");

        assert_eq!(parsed.status, "failed");
        assert_eq!(parsed.started_at, "2026-02-17T00:00:00Z");
        assert_eq!(parsed.profile, "");
        assert_eq!(parsed.output, "");
        assert_eq!(parsed.seed_demo, 0);
        assert_eq!(parsed.snapshot_required, 0);
        assert!(parsed.finished_at.is_none());
    }

    #[test]
    fn decision_record_append_jsonl_writes_one_json_object_per_line() {
        let temp = tempdir().expect("tempdir");
        let ledger = temp.path().join("ledger.jsonl");

        let first = DecisionRecord {
            timestamp: "2026-02-17T00:00:00Z".to_string(),
            trace_id: "trace-1".to_string(),
            decision_id: "decision-1".to_string(),
            action: "capture_config_resolved".to_string(),
            evidence_terms: vec!["profile=analytics-empty".to_string()],
            fallback_active: false,
            fallback_reason: None,
            policy_id: "doctor_frankentui/v1".to_string(),
        };
        let second = DecisionRecord {
            timestamp: "2026-02-17T00:00:01Z".to_string(),
            trace_id: "trace-1".to_string(),
            decision_id: "decision-2".to_string(),
            action: "capture_finalize".to_string(),
            evidence_terms: vec!["final_status=ok".to_string()],
            fallback_active: true,
            fallback_reason: Some("capture timeout exceeded 30s".to_string()),
            policy_id: "doctor_frankentui/v1".to_string(),
        };

        first.append_jsonl(&ledger).expect("append first record");
        second.append_jsonl(&ledger).expect("append second record");

        let content = std::fs::read_to_string(&ledger).expect("read ledger");
        let lines = content.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 2);

        let parsed_first =
            serde_json::from_str::<DecisionRecord>(lines[0]).expect("parse first decision");
        let parsed_second =
            serde_json::from_str::<DecisionRecord>(lines[1]).expect("parse second decision");

        assert_eq!(parsed_first.decision_id, "decision-1");
        assert_eq!(parsed_second.decision_id, "decision-2");
        assert!(parsed_second.fallback_active);
    }

    #[test]
    fn runmeta_write_to_path_creates_parent_directories() {
        let temp = tempdir().expect("tempdir");
        let nested = temp.path().join("a/b/run_meta.json");

        let meta = RunMeta {
            status: "ok".to_string(),
            started_at: "2026-02-17T00:00:00Z".to_string(),
            profile: "analytics-empty".to_string(),
            ..RunMeta::default()
        };

        meta.write_to_path(&nested).expect("write nested run_meta");
        assert!(nested.exists());

        let decoded = RunMeta::from_path(&nested).expect("read nested run_meta");
        assert_eq!(decoded.status, meta.status);
        assert_eq!(decoded.profile, meta.profile);
    }
}
