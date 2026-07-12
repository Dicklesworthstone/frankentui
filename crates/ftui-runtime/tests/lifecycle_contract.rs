//! Cross-component lifecycle contract tests (bd-1dg21).
//!
//! These integration tests capture the observable behavioral contract of the
//! runtime lifecycle that MUST be preserved during the Asupersync migration.
//! They test through public APIs only (ProgramSimulator, CancellationToken,
//! effect system metrics). Tests requiring internal APIs (StopSignal::new,
//! SubscriptionManager) live in the crate-internal unit test modules.

#![forbid(unsafe_code)]

use ftui_backend::{BackendEventSource, BackendFeatures};
use ftui_core::event::Event;
use ftui_core::terminal_capabilities::TerminalCapabilities;
use ftui_render::frame::Frame;
use ftui_runtime::program::{Cmd, Model, Program, ProgramConfig};
use ftui_runtime::simulator::{ProgramSimulator, SimulatorError};
use ftui_runtime::subscription::{StopSignal, SubId, Subscription};
use ftui_runtime::terminal_writer::{ScreenMode, TerminalWriter, UiAnchor};
use std::io;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

// ============================================================================
// Test model: LifecycleTracker
//
// Records the exact sequence of lifecycle events to verify ordering contracts.
// ============================================================================

struct LifecycleTracker {
    trace: Vec<String>,
}

#[derive(Debug)]
enum LMsg {
    Init,
    Tick,
    Quit,
    TaskResult(String),
    SpawnTask,
}

impl From<Event> for LMsg {
    fn from(_: Event) -> Self {
        LMsg::Tick
    }
}

impl Model for LifecycleTracker {
    type Message = LMsg;

    fn init(&mut self) -> Cmd<Self::Message> {
        self.trace.push("init".into());
        Cmd::msg(LMsg::Init)
    }

    fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
        match msg {
            LMsg::Init => {
                self.trace.push("update:init".into());
                Cmd::none()
            }
            LMsg::Tick => {
                self.trace.push("update:tick".into());
                Cmd::none()
            }
            LMsg::Quit => {
                self.trace.push("update:quit".into());
                Cmd::quit()
            }
            LMsg::TaskResult(s) => {
                self.trace.push(format!("update:task-result:{s}"));
                Cmd::none()
            }
            LMsg::SpawnTask => {
                self.trace.push("update:spawn-task".into());
                Cmd::task(|| LMsg::TaskResult("done".into()))
            }
        }
    }

    fn view(&self, _frame: &mut Frame) {}

    fn on_shutdown(&mut self) -> Cmd<Self::Message> {
        self.trace.push("on_shutdown".into());
        Cmd::none()
    }
}

// ============================================================================
// CONTRACT: Full lifecycle trace ordering
// ============================================================================

/// CONTRACT: The lifecycle must follow: init -> update(init_cmd) -> updates
/// -> quit -> on_shutdown. This ordering is critical for the Asupersync
/// migration to preserve.
#[test]
fn contract_lifecycle_ordering_init_update_shutdown() {
    let mut sim = ProgramSimulator::new(LifecycleTracker { trace: vec![] });

    sim.init();
    sim.send(LMsg::SpawnTask);
    sim.send(LMsg::Quit);
    sim.shutdown();

    let trace = &sim.model().trace;
    assert_eq!(
        trace,
        &[
            "init",
            "update:init",
            "update:spawn-task",
            "update:task-result:done",
            "update:quit",
            "on_shutdown",
        ],
        "lifecycle must follow: init -> update(init_cmd) -> updates -> quit -> on_shutdown"
    );
}

/// CONTRACT: Cmd::Task results are routed through Model::update() synchronously
/// in the simulator, and the ordering is deterministic.
#[test]
fn contract_task_result_ordering_is_deterministic() {
    struct MultiTaskModel {
        trace: Vec<String>,
    }

    #[derive(Debug)]
    enum MTMsg {
        SpawnAll,
        Result(String),
    }

    impl From<Event> for MTMsg {
        fn from(_: Event) -> Self {
            MTMsg::SpawnAll
        }
    }

    impl Model for MultiTaskModel {
        type Message = MTMsg;

        fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
            match msg {
                MTMsg::SpawnAll => {
                    self.trace.push("spawn-all".into());
                    Cmd::batch(vec![
                        Cmd::task(|| MTMsg::Result("task-a".into())),
                        Cmd::task(|| MTMsg::Result("task-b".into())),
                        Cmd::task(|| MTMsg::Result("task-c".into())),
                    ])
                }
                MTMsg::Result(s) => {
                    self.trace.push(format!("result:{s}"));
                    Cmd::none()
                }
            }
        }

        fn view(&self, _frame: &mut Frame) {}
    }

    // Run twice and verify deterministic ordering
    for _ in 0..3 {
        let mut sim = ProgramSimulator::new(MultiTaskModel { trace: vec![] });
        sim.init();
        sim.send(MTMsg::SpawnAll);

        assert_eq!(
            sim.model().trace,
            vec![
                "spawn-all",
                "result:task-a",
                "result:task-b",
                "result:task-c",
            ],
            "task results must be processed in batch submission order"
        );
    }
}

/// CONTRACT: Cmd::Batch stops executing after Cmd::Quit.
#[test]
fn contract_batch_halts_on_quit() {
    struct HaltModel {
        steps: Vec<&'static str>,
    }

    #[derive(Debug)]
    enum HMsg {
        Step(&'static str),
        TriggerBatchWithQuit,
    }

    impl From<Event> for HMsg {
        fn from(_: Event) -> Self {
            HMsg::Step("event")
        }
    }

    impl Model for HaltModel {
        type Message = HMsg;

        fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
            match msg {
                HMsg::Step(s) => {
                    self.steps.push(s);
                    Cmd::none()
                }
                HMsg::TriggerBatchWithQuit => Cmd::batch(vec![
                    Cmd::msg(HMsg::Step("before-quit")),
                    Cmd::quit(),
                    Cmd::msg(HMsg::Step("after-quit")),
                ]),
            }
        }

        fn view(&self, _frame: &mut Frame) {}
    }

    let mut sim = ProgramSimulator::new(HaltModel { steps: vec![] });
    sim.init();
    sim.send(HMsg::TriggerBatchWithQuit);

    assert!(!sim.is_running());
    assert_eq!(
        sim.model().steps,
        vec!["before-quit"],
        "commands after Quit in a Batch must not execute"
    );
}

/// CONTRACT: Cmd::Sequence stops executing after Cmd::Quit.
#[test]
fn contract_sequence_halts_on_quit() {
    struct SeqModel {
        steps: Vec<&'static str>,
    }

    #[derive(Debug)]
    enum SMsg {
        Step(&'static str),
        TriggerSeqWithQuit,
    }

    impl From<Event> for SMsg {
        fn from(_: Event) -> Self {
            SMsg::Step("event")
        }
    }

    impl Model for SeqModel {
        type Message = SMsg;

        fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
            match msg {
                SMsg::Step(s) => {
                    self.steps.push(s);
                    Cmd::none()
                }
                SMsg::TriggerSeqWithQuit => Cmd::sequence(vec![
                    Cmd::msg(SMsg::Step("before-quit")),
                    Cmd::quit(),
                    Cmd::msg(SMsg::Step("after-quit")),
                ]),
            }
        }

        fn view(&self, _frame: &mut Frame) {}
    }

    let mut sim = ProgramSimulator::new(SeqModel { steps: vec![] });
    sim.init();
    sim.send(SMsg::TriggerSeqWithQuit);

    assert!(!sim.is_running());
    assert_eq!(
        sim.model().steps,
        vec!["before-quit"],
        "commands after Quit in a Sequence must not execute"
    );
}

// ============================================================================
// CONTRACT: Program error and shutdown hooks
// ============================================================================

#[derive(Debug)]
enum HookMsg {
    ErrorCommand,
    ShutdownCommand(&'static str),
    Noop,
}

impl From<Event> for HookMsg {
    fn from(_: Event) -> Self {
        Self::Noop
    }
}

struct HookModel {
    trace: Vec<String>,
}

impl Model for HookModel {
    type Message = HookMsg;

    fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
        match msg {
            HookMsg::ErrorCommand => self.trace.push("error-command".into()),
            HookMsg::ShutdownCommand(tag) => {
                self.trace.push(format!("shutdown-command:{tag}"));
            }
            HookMsg::Noop => self.trace.push("noop".into()),
        }
        Cmd::none()
    }

    fn view(&self, _frame: &mut Frame) {}

    fn on_error(&mut self, error: &str) -> Cmd<Self::Message> {
        self.trace.push(format!("error:{error}"));
        Cmd::msg(HookMsg::ErrorCommand)
    }

    fn on_shutdown(&mut self) -> Cmd<Self::Message> {
        self.trace.push("shutdown".into());
        Cmd::batch(vec![
            Cmd::msg(HookMsg::ShutdownCommand("one")),
            Cmd::msg(HookMsg::ShutdownCommand("two")),
        ])
    }
}

struct PollFailureSource;

impl BackendEventSource for PollFailureSource {
    type Error = io::Error;

    fn size(&self) -> Result<(u16, u16), Self::Error> {
        Ok((20, 5))
    }

    fn set_features(&mut self, _features: BackendFeatures) -> Result<(), Self::Error> {
        Ok(())
    }

    fn poll_event(&mut self, _timeout: Duration) -> Result<bool, Self::Error> {
        Err(io::Error::other("poll failed"))
    }

    fn read_event(&mut self) -> Result<Option<Event>, Self::Error> {
        Ok(None)
    }
}

fn lifecycle_config() -> ProgramConfig {
    let mut config = ProgramConfig::default().with_forced_size(20, 5);
    config.screen_mode = ScreenMode::AltScreen;
    config.poll_timeout = Duration::ZERO;
    config.intercept_signals = false;
    config
}

#[test]
fn contract_runtime_error_calls_error_then_shutdown_once() {
    let config = lifecycle_config();
    let writer = TerminalWriter::new(
        Vec::new(),
        config.screen_mode,
        UiAnchor::Bottom,
        TerminalCapabilities::basic(),
    );
    let mut program = Program::with_event_source(
        HookModel { trace: Vec::new() },
        PollFailureSource,
        BackendFeatures::default(),
        writer,
        config,
    )
    .expect("program construction");

    let error = program.run().expect_err("poll failure must be returned");
    assert_eq!(error.kind(), io::ErrorKind::Other);
    assert!(error.to_string().contains("poll failed"));
    assert_eq!(
        program.model().trace,
        vec![
            "error:poll failed",
            "error-command",
            "shutdown",
            "shutdown-command:one",
            "shutdown-command:two",
        ]
    );

    let second = program.run().expect_err("completed lifecycle cannot rerun");
    assert_eq!(second.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(
        program
            .model()
            .trace
            .iter()
            .filter(|entry| entry.as_str() == "shutdown")
            .count(),
        1
    );
}

struct FailingWriter;

impl Write for FailingWriter {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        Err(io::Error::new(io::ErrorKind::BrokenPipe, "writer failed"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::BrokenPipe, "writer failed"))
    }
}

struct CommandFailureModel {
    errors: usize,
    shutdowns: usize,
}

impl Model for CommandFailureModel {
    type Message = HookMsg;

    fn init(&mut self) -> Cmd<Self::Message> {
        Cmd::log("force command write")
    }

    fn update(&mut self, _msg: Self::Message) -> Cmd<Self::Message> {
        Cmd::none()
    }

    fn view(&self, _frame: &mut Frame) {}

    fn on_error(&mut self, error: &str) -> Cmd<Self::Message> {
        assert!(error.contains("writer failed"));
        self.errors += 1;
        Cmd::none()
    }

    fn on_shutdown(&mut self) -> Cmd<Self::Message> {
        self.shutdowns += 1;
        Cmd::none()
    }
}

#[test]
fn contract_command_error_calls_error_and_shutdown_hooks() {
    let config = lifecycle_config();
    let writer = TerminalWriter::new(
        FailingWriter,
        config.screen_mode,
        UiAnchor::Bottom,
        TerminalCapabilities::basic(),
    );
    let mut program = Program::with_event_source(
        CommandFailureModel {
            errors: 0,
            shutdowns: 0,
        },
        PollFailureSource,
        BackendFeatures::default(),
        writer,
        config,
    )
    .expect("program construction");

    let error = program.run().expect_err("command write must fail");
    assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
    assert_eq!(program.model().errors, 1);
    assert_eq!(program.model().shutdowns, 1);
}

struct PanicSubscription;

impl Subscription<HookMsg> for PanicSubscription {
    fn id(&self) -> SubId {
        77
    }

    fn run(&self, _sender: std::sync::mpsc::Sender<HookMsg>, _stop: StopSignal) {
        panic!("subscription exploded");
    }
}

struct BoundedIdleSource {
    polls: usize,
}

impl BackendEventSource for BoundedIdleSource {
    type Error = io::Error;

    fn size(&self) -> Result<(u16, u16), Self::Error> {
        Ok((20, 5))
    }

    fn set_features(&mut self, _features: BackendFeatures) -> Result<(), Self::Error> {
        Ok(())
    }

    fn poll_event(&mut self, _timeout: Duration) -> Result<bool, Self::Error> {
        self.polls += 1;
        if self.polls > 200 {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "subscription failure was not delivered",
            ));
        }
        std::thread::sleep(Duration::from_millis(1));
        Ok(false)
    }

    fn read_event(&mut self) -> Result<Option<Event>, Self::Error> {
        Ok(None)
    }
}

struct SubscriptionFailureModel {
    error: Option<String>,
    shutdowns: usize,
}

impl Model for SubscriptionFailureModel {
    type Message = HookMsg;

    fn update(&mut self, _msg: Self::Message) -> Cmd<Self::Message> {
        Cmd::none()
    }

    fn view(&self, _frame: &mut Frame) {}

    fn subscriptions(&self) -> Vec<Box<dyn Subscription<Self::Message>>> {
        if self.error.is_none() {
            vec![Box::new(PanicSubscription)]
        } else {
            Vec::new()
        }
    }

    fn on_error(&mut self, error: &str) -> Cmd<Self::Message> {
        self.error = Some(error.to_owned());
        Cmd::quit()
    }

    fn on_shutdown(&mut self) -> Cmd<Self::Message> {
        self.shutdowns += 1;
        Cmd::none()
    }
}

#[test]
fn contract_subscription_failure_calls_error_and_shutdown_hooks() {
    let config = lifecycle_config();
    let writer = TerminalWriter::new(
        Vec::new(),
        config.screen_mode,
        UiAnchor::Bottom,
        TerminalCapabilities::basic(),
    );
    let mut program = Program::with_event_source(
        SubscriptionFailureModel {
            error: None,
            shutdowns: 0,
        },
        BoundedIdleSource { polls: 0 },
        BackendFeatures::default(),
        writer,
        config,
    )
    .expect("program construction");

    program
        .run()
        .expect("isolated subscription error should recover through Cmd::Quit");
    let error = program.model().error.as_deref().expect("error hook ran");
    assert!(error.contains("subscription 77 failed"));
    assert!(error.contains("subscription exploded"));
    assert_eq!(program.model().shutdowns, 1);
}

// ============================================================================
// CONTRACT: Deterministic ProgramSimulator lifecycle support
// ============================================================================

const SIM_SUB_ID: SubId = 4242;

struct DormantSubscription;

impl Subscription<SimMsg> for DormantSubscription {
    fn id(&self) -> SubId {
        SIM_SUB_ID
    }

    fn run(&self, _sender: std::sync::mpsc::Sender<SimMsg>, _stop: StopSignal) {}
}

#[derive(Debug)]
enum SimMsg {
    Tick,
    Delivered,
    Deactivate,
    ErrorHandled,
    ShutdownStep(&'static str),
    Quit,
}

impl From<Event> for SimMsg {
    fn from(event: Event) -> Self {
        match event {
            Event::Tick => Self::Tick,
            _ => Self::Delivered,
        }
    }
}

struct SimulatorLifecycleModel {
    ticks: usize,
    deliveries: usize,
    active: bool,
    errors: usize,
    shutdowns: usize,
    shutdown_steps: Vec<&'static str>,
}

impl Model for SimulatorLifecycleModel {
    type Message = SimMsg;

    fn init(&mut self) -> Cmd<Self::Message> {
        Cmd::tick(Duration::from_millis(10))
    }

    fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
        match msg {
            SimMsg::Tick => self.ticks += 1,
            SimMsg::Delivered => self.deliveries += 1,
            SimMsg::Deactivate => self.active = false,
            SimMsg::ErrorHandled => self.errors += 1,
            SimMsg::ShutdownStep(step) => self.shutdown_steps.push(step),
            SimMsg::Quit => return Cmd::quit(),
        }
        Cmd::none()
    }

    fn view(&self, _frame: &mut Frame) {}

    fn subscriptions(&self) -> Vec<Box<dyn Subscription<Self::Message>>> {
        if self.active {
            vec![Box::new(DormantSubscription)]
        } else {
            Vec::new()
        }
    }

    fn on_error(&mut self, _error: &str) -> Cmd<Self::Message> {
        Cmd::msg(SimMsg::ErrorHandled)
    }

    fn on_shutdown(&mut self) -> Cmd<Self::Message> {
        self.shutdowns += 1;
        Cmd::batch(vec![
            Cmd::msg(SimMsg::ShutdownStep("one")),
            Cmd::msg(SimMsg::ShutdownStep("two")),
        ])
    }
}

fn simulator_lifecycle_model() -> SimulatorLifecycleModel {
    SimulatorLifecycleModel {
        ticks: 0,
        deliveries: 0,
        active: true,
        errors: 0,
        shutdowns: 0,
        shutdown_steps: Vec::new(),
    }
}

#[test]
fn contract_simulator_injects_time_and_subscription_delivery() {
    let mut sim = ProgramSimulator::new(simulator_lifecycle_model());
    sim.init();
    assert_eq!(sim.active_subscription_ids(), &[SIM_SUB_ID]);

    assert_eq!(sim.advance_time(Duration::from_millis(35)), 3);
    assert_eq!(sim.now(), Duration::from_millis(35));
    assert_eq!(sim.model().ticks, 3);

    sim.deliver_subscription(SIM_SUB_ID, SimMsg::Delivered)
        .expect("active subscription delivery");
    assert_eq!(sim.model().deliveries, 1);

    sim.deliver_subscription(SIM_SUB_ID, SimMsg::Deactivate)
        .expect("deactivation delivery");
    assert!(sim.active_subscription_ids().is_empty());

    let error = sim
        .deliver_subscription(SIM_SUB_ID, SimMsg::Delivered)
        .expect_err("inactive subscription must be rejected");
    assert_eq!(error, SimulatorError::InactiveSubscription(SIM_SUB_ID));
    assert_eq!(sim.model().errors, 1);
    assert_eq!(sim.errors().len(), 1);
}

#[test]
fn contract_simulator_shutdown_and_cancel_are_exactly_once() {
    let mut sim = ProgramSimulator::new(simulator_lifecycle_model());
    sim.init();
    sim.send(SimMsg::Quit);
    sim.shutdown();
    sim.shutdown();
    sim.cancel();

    assert!(sim.is_shutdown());
    assert!(!sim.is_running());
    assert_eq!(sim.model().shutdowns, 1);
    assert_eq!(sim.model().shutdown_steps, vec!["one", "two"]);
    assert!(sim.active_subscription_ids().is_empty());
}

// ============================================================================
// CONTRACT: CancellationToken integration
// ============================================================================

/// CONTRACT: CancellationToken must cooperatively stop background work.
/// cancel() must wake threads blocked in wait_timeout().
#[test]
fn contract_cancellation_token_stops_background_work() {
    use ftui_runtime::cancellation::CancellationSource;

    let source = CancellationSource::new();
    let token = source.token();
    let work_done = Arc::new(AtomicUsize::new(0));
    let work_clone = work_done.clone();

    let handle = std::thread::spawn(move || {
        while !token.is_cancelled() {
            work_clone.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(10));
        }
    });

    std::thread::sleep(Duration::from_millis(50));
    let before_cancel = work_done.load(Ordering::SeqCst);
    assert!(before_cancel > 0, "work should have started");

    source.cancel();
    handle.join().unwrap();

    let at_cancel = work_done.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(50));
    let after_wait = work_done.load(Ordering::SeqCst);
    assert_eq!(
        at_cancel, after_wait,
        "no work should happen after cancellation"
    );
}

/// CONTRACT: Dropping CancellationSource does NOT cancel the token.
/// Cancellation must be explicit.
#[test]
fn contract_cancellation_drop_does_not_cancel() {
    use ftui_runtime::cancellation::CancellationSource;

    let source = CancellationSource::new();
    let token = source.token();
    drop(source);
    assert!(
        !token.is_cancelled(),
        "dropping source must not cancel token"
    );
}

// ============================================================================
// CONTRACT: Effect system metrics
// ============================================================================

/// CONTRACT: Effect counters are monotonic and increment by exactly 1 per call.
/// The combined total must always equal command_total + subscription_total.
#[test]
fn contract_effect_metrics_increment_monotonically() {
    let before_cmd = ftui_runtime::effect_system::effects_command_total();
    let before_sub = ftui_runtime::effect_system::effects_subscription_total();

    ftui_runtime::effect_system::record_command_effect("test_cmd", 100);
    ftui_runtime::effect_system::record_command_effect("test_cmd", 200);

    let after_cmd = ftui_runtime::effect_system::effects_command_total();
    assert_eq!(
        after_cmd - before_cmd,
        2,
        "command counter must increment by exactly 1 per call"
    );

    ftui_runtime::effect_system::record_subscription_start("test_sub", 1);

    let after_sub = ftui_runtime::effect_system::effects_subscription_total();
    assert_eq!(
        after_sub - before_sub,
        1,
        "subscription counter must increment by exactly 1 per call"
    );

    let total = ftui_runtime::effect_system::effects_executed_total();
    assert_eq!(
        total,
        after_cmd + after_sub,
        "combined total must equal cmd + sub"
    );
}

/// CONTRACT: No messages are processed after Cmd::Quit.
#[test]
fn contract_no_processing_after_quit() {
    struct CountModel {
        value: i32,
    }

    #[derive(Debug)]
    enum CMsg {
        Inc,
        Quit,
    }

    impl From<Event> for CMsg {
        fn from(_: Event) -> Self {
            CMsg::Inc
        }
    }

    impl Model for CountModel {
        type Message = CMsg;

        fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
            match msg {
                CMsg::Inc => {
                    self.value += 1;
                    Cmd::none()
                }
                CMsg::Quit => Cmd::quit(),
            }
        }

        fn view(&self, _frame: &mut Frame) {}
    }

    let mut sim = ProgramSimulator::new(CountModel { value: 0 });
    sim.init();

    sim.send(CMsg::Inc); // value = 1
    sim.send(CMsg::Quit);
    sim.send(CMsg::Inc); // must be ignored
    sim.send(CMsg::Inc); // must be ignored

    assert_eq!(sim.model().value, 1, "messages after Quit must be ignored");
    assert!(!sim.is_running());
}
