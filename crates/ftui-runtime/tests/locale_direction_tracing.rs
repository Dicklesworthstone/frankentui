#![forbid(unsafe_code)]

//! Isolated integration test for `LocaleContext::set_direction` tracing events.
//!
//! Lives in its own integration-test binary so no concurrent unit test can hit
//! the same `tracing::info!` callsite without a subscriber and poison the
//! callsite interest cache (fixes #103).

use std::sync::{Arc, Mutex};

use ftui_runtime::locale::{LocaleContext, TextDirection};
use ftui_runtime::telemetry_schema::TARGET_LOCALE;
use tracing_subscriber::prelude::*;

#[derive(Clone, Default)]
struct LocaleTraceCapture(Arc<Mutex<Vec<(String, String, String)>>>);

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for LocaleTraceCapture {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        if event.metadata().target() == TARGET_LOCALE {
            struct Visitor {
                locale: String,
                direction: String,
                source: String,
            }

            impl tracing::field::Visit for Visitor {
                fn record_debug(
                    &mut self,
                    field: &tracing::field::Field,
                    value: &dyn std::fmt::Debug,
                ) {
                    let val_str = format!("{value:?}");
                    let val_trimmed = val_str.trim_matches('"').to_string();
                    match field.name() {
                        "locale" => self.locale = val_trimmed,
                        "direction" => self.direction = val_str,
                        "source" => self.source = val_trimmed,
                        _ => {}
                    }
                }

                fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
                    match field.name() {
                        "locale" => self.locale = value.to_string(),
                        "source" => self.source = value.to_string(),
                        _ => {}
                    }
                }
            }

            let mut visitor = Visitor {
                locale: String::new(),
                direction: String::new(),
                source: String::new(),
            };
            event.record(&mut visitor);
            self.0.lock().expect("capture lock").push((
                visitor.locale,
                visitor.direction,
                visitor.source,
            ));
        }
    }
}

#[test]
fn set_direction_emits_locale_tracing_events_in_isolation() {
    let capture = LocaleTraceCapture::default();
    let subscriber = tracing_subscriber::registry().with(capture.clone());

    tracing::subscriber::with_default(subscriber, || {
        let ctx = LocaleContext::new("en");
        assert_eq!(ctx.direction(), TextDirection::Ltr);

        ctx.set_direction(Some(TextDirection::Rtl));
        assert_eq!(ctx.direction(), TextDirection::Rtl);

        ctx.set_direction(None);
        assert_eq!(ctx.direction(), TextDirection::Ltr);

        ctx.set_locale("ar");
        assert_eq!(ctx.direction(), TextDirection::Rtl);
    });

    let events = capture.0.lock().expect("capture lock");
    assert_eq!(
        events.len(),
        3,
        "Expected override, revert, and locale-switch tracing events from {TARGET_LOCALE}, got {events:?}"
    );
    assert_eq!(events[0].0, "en");
    assert!(events[0].1.contains("Rtl"));
    assert_eq!(events[0].2, "override");

    assert_eq!(events[1].0, "en");
    assert!(events[1].1.contains("Ltr"));
    assert_eq!(events[1].2, "override");

    assert_eq!(events[2].0, "ar");
    assert!(events[2].1.contains("Rtl"));
    assert_eq!(events[2].2, "locale");
}
