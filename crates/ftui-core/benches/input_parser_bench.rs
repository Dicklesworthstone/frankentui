//! Terminal input parsing and dispatch throughput (bd-g00-root-epic-ewths.31.2).
//!
//! Run with: cargo bench -p ftui-core --bench input_parser_bench
//!
//! Every keystroke, mouse move and paste arrives as bytes that the parser has
//! to disambiguate, often without knowing whether more bytes are coming. Two
//! things are worth measuring separately:
//!
//! - **per-event cost over a realistic mix**, because the parser's state
//!   machine takes very different paths for a plain key, a CSI sequence and a
//!   kitty `CSI u` key, and an average over one kind tells you nothing about a
//!   real session;
//! - **one pathological paste**, because bracketed paste is the input that can
//!   arrive in tens of kilobytes at once and is where quadratic buffer
//!   handling would show up.
//!
//! The corpus is built from fixed formulas, not random bytes: a bench whose
//! input changes between runs cannot detect a regression.

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use ftui_core::event::Event;
use ftui_core::input_parser::InputParser;
use std::hint::black_box;

/// Number of encoded events in the mixed corpus.
const MIXED_EVENT_COUNT: usize = 1000;

/// A counter model standing in for `Model::update`.
///
/// Dispatch is included in the mixed-stream measurement because parsing an
/// event nobody consumes is not the operation the runtime actually performs;
/// the match over `Event` is part of the per-event cost. The work per event is
/// deliberately trivial so the number stays dominated by parse and dispatch
/// rather than by whatever a real model does.
#[derive(Default)]
struct CounterModel {
    keys: u64,
    mice: u64,
    pastes: u64,
    focus: u64,
    other: u64,
}

impl CounterModel {
    #[inline]
    fn update(&mut self, event: &Event) {
        match event {
            Event::Key(_) => self.keys += 1,
            Event::Mouse(_) => self.mice += 1,
            Event::Paste(_) => self.pastes += 1,
            Event::Focus(_) => self.focus += 1,
            _ => self.other += 1,
        }
    }
}

/// A byte corpus of `MIXED_EVENT_COUNT` encoded events, cycling through the
/// encodings a real session produces.
///
/// The cycle length is coprime with the number of shapes so the sequence does
/// not settle into a repeating pair, which would let the branch predictor make
/// the parser look faster than it is.
fn mixed_event_stream() -> (Vec<u8>, usize) {
    let mut out = Vec::with_capacity(64 * 1024);
    let mut events = 0;

    while events < MIXED_EVENT_COUNT {
        match events % 11 {
            // Plain printable keys.
            0..=2 => {
                out.push(b'a' + (events % 26) as u8);
                events += 1;
            }
            // CSI arrows.
            3..=4 => {
                out.extend_from_slice(b"\x1b[A");
                events += 1;
            }
            // SS3 function keys.
            5 => {
                out.extend_from_slice(b"\x1bOP");
                events += 1;
            }
            // Kitty `CSI u` with modifiers (ctrl+shift+a).
            6 => {
                out.extend_from_slice(b"\x1b[97;6u");
                events += 1;
            }
            // SGR mouse press.
            7 => {
                out.extend_from_slice(b"\x1b[<0;10;5M");
                events += 1;
            }
            // Focus in / focus out.
            8 => {
                out.extend_from_slice(b"\x1b[I");
                out.extend_from_slice(b"\x1b[O");
                events += 2;
            }
            // A 2 KB bracketed paste.
            9 => {
                out.extend_from_slice(b"\x1b[200~");
                for i in 0..2048 {
                    out.push(b'a' + (i % 26) as u8);
                }
                out.extend_from_slice(b"\x1b[201~");
                events += 1;
            }
            // A 200-byte OSC reply, which the parser must consume without
            // emitting a key for every byte of it.
            _ => {
                out.extend_from_slice(b"\x1b]11;rgb:");
                for i in 0..180 {
                    out.push(b'0' + (i % 10) as u8);
                }
                out.push(0x07);
                events += 1;
            }
        }
    }

    (out, events)
}

/// One 64 KB bracketed paste, wrapped in its start and end markers.
fn paste_64kb() -> Vec<u8> {
    let mut out = Vec::with_capacity(64 * 1024 + 16);
    out.extend_from_slice(b"\x1b[200~");
    for i in 0..(64 * 1024) {
        out.push(b'a' + (i % 26) as u8);
    }
    out.extend_from_slice(b"\x1b[201~");
    out
}

fn bench_input(c: &mut Criterion) {
    let (stream, encoded_events) = mixed_event_stream();
    let paste = paste_64kb();

    let mut group = c.benchmark_group("input");

    // Elements, not bytes: the interesting figure is cost per event, and the
    // corpus deliberately mixes one-byte keys with a 2 KB paste.
    group.throughput(Throughput::Elements(encoded_events as u64));
    group.bench_function("parse_dispatch/mixed_event_stream/per_event", |b| {
        b.iter(|| {
            // A fresh parser per iteration: parser state carries across calls,
            // and reusing one would measure a warmed-up machine that the real
            // startup path never has.
            let mut parser = InputParser::new();
            let mut model = CounterModel::default();
            let mut events = Vec::with_capacity(MIXED_EVENT_COUNT);
            parser.parse_into(black_box(&stream), &mut events);
            for event in &events {
                model.update(event);
            }
            black_box(model.keys + model.mice + model.pastes + model.focus + model.other)
        });
    });
    group.finish();

    let mut paste_group = c.benchmark_group("input");
    paste_group.throughput(Throughput::Bytes(paste.len() as u64));
    paste_group.bench_function("parse/paste_64kb", |b| {
        b.iter(|| {
            let mut parser = InputParser::new();
            black_box(parser.parse(black_box(&paste)))
        });
    });
    paste_group.finish();
}

criterion_group!(benches, bench_input);
criterion_main!(benches);
