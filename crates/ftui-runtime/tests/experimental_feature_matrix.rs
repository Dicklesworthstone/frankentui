#![cfg(feature = "experimental")]

use ftui_runtime::*;

#[test]
fn all_quarantined_modules_are_nameable() {
    // 2. alpha_investing
    let _: Option<alpha_investing::AlphaInvestingConfig> = None;
    // 3. conformal_alert
    let _: Option<conformal_alert::AlertConfig> = None;
    // 4. conformal_frame_guard
    let _: Option<conformal_frame_guard::ConformalFrameGuardConfig> = None;
    // 5. conformal_stages
    let _: Option<conformal_stages::RenderStage> = None;
    // 6. cost_model
    let _: Option<cost_model::CacheCostParams> = None;
    // 7. countmin_sketch
    let _: Option<countmin_sketch::CountMinSketch> = None;
    // 8. degradation_cascade
    let _: Option<degradation_cascade::CascadeConfig> = None;
    // 10. eprocess_throttle
    let _: Option<eprocess_throttle::ThrottleConfig> = None;
    // 11. evidence_bridges
    let _ = evidence_bridges::from_diff_strategy;
    // 12. flake_detector
    let _: Option<flake_detector::FlakeConfig> = None;
    // 13. flat_combine
    let _: Option<flat_combine::CombinerStats> = None;
    // 14. ivm
    let _: Option<ivm::ViewId> = None;
    // 15. lens
    let l1 = lens::field_lens(|s: &i32| *s, |s: &mut i32, v| *s = v);
    let l2 = lens::field_lens(|s: &i32| *s, |s: &mut i32, v| *s = v);
    let _ = lens::compose(l1, l2);
    // 16. policy_config
    let _: Option<policy_config::PolicyConfig> = None;
    // 17. policy_registry
    let _: Option<policy_registry::PolicyRegistry> = None;
    // 18. resize_sla
    let _: Option<resize_sla::SlaConfig> = None;
    // 19. reversible
    let _: Option<reversible::AddOp<i32>> = None;
    // 20. rough_path
    let _: Option<rough_path::SignatureConfig> = None;
    // 21. slo
    let _: Option<slo::MetricType> = None;
    // 22. sos_barrier
    let _ = sos_barrier::evaluate as fn(f64, f64) -> sos_barrier::BarrierResult;
    // 23. timeline_aggregator
    let _: Option<timeline_aggregator::TimelineAggregator> = None;
    // 24. validation_pipeline
    let _: Option<validation_pipeline::PipelineConfig> = None;
    // 25. wasm_runner
    let _: Option<wasm_runner::StepResult> = None;
    // 26. schedule_trace
    let _: Option<schedule_trace::TaskEvent> = None;
}
