//! Profile sweep binary for flamegraph / heaptrack analysis (bd-3jlw5.7, bd-3jlw5.8).
//!
//! Renders every demo screen at 80x24 and 120x40 in a tight loop.
//! Designed to be run under `cargo flamegraph` or `heaptrack`:
//!
//!   cargo flamegraph --bin profile_sweep -p ftui-demo-showcase -- --cycles 100
//!   heaptrack cargo run --release --bin profile_sweep -p ftui-demo-showcase -- --cycles 10

use std::time::Instant;

use ftui_core::event::Event;
use ftui_demo_showcase::app::AppModel;
use ftui_demo_showcase::screens;
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;
use ftui_runtime::{Cmd, Model};

fn main() {
    let cycles: usize = std::env::args()
        .skip_while(|a| a != "--cycles")
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(50);

    let sizes: &[(u16, u16)] = &[(80, 24), (120, 40)];
    let screen_ids = screens::screen_ids();

    eprintln!(
        "Profile sweep: {} screens x {} sizes x {} cycles = {} renders",
        screen_ids.len(),
        sizes.len(),
        cycles,
        screen_ids.len() * sizes.len() * cycles
    );

    let start = Instant::now();
    let mut pool = GraphemePool::new();

    for &(cols, rows) in sizes {
        let mut app = AppModel::new();
        let _: Cmd<_> = app.init();
        let _: Cmd<_> = app.update(Event::Tick.into());

        for cycle in 0..cycles {
            for &screen in screen_ids.iter() {
                app.current_screen = screen;
                let _: Cmd<_> = app.update(Event::Tick.into());
                let mut frame = Frame::new(cols, rows, &mut pool);
                app.view(&mut frame);
                // Ensure the optimizer doesn't elide the render.
                std::hint::black_box(&frame);
            }
            if cycle % 10 == 0 {
                eprint!(".");
            }
        }
    }

    let elapsed = start.elapsed();
    let total = screen_ids.len() * sizes.len() * cycles;
    eprintln!(
        "\nDone in {:.2}s ({:.1} renders/sec)",
        elapsed.as_secs_f64(),
        total as f64 / elapsed.as_secs_f64()
    );
}
