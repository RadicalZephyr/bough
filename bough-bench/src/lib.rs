//! Benchmark shapes and baselines (RFD 1, "What Fast Means").
//!
//! Each shape is a graph plus a driver, next to a hand-written imperative
//! baseline doing the same work. The bar is a factor of three against the
//! baseline on realistic per-node payloads of 50 to 100 nanoseconds; the
//! trivial-payload numbers are reported as information. The UI shape needs
//! switches and lands with them.

use std::cell::Cell as StdCell;
use std::hint::black_box;
use std::rc::Rc;

use bough::{Cell, Input, Listener, Runtime, Shared, Source};

/// Rounds of [`payload`]: about 55 ns of the user's own work on the machine
/// the stage 1 bar was measured on.
pub const PAYLOAD_ROUNDS: u32 = 50;

/// A stand-in for the user's own work in a node function: a dependent chain
/// of multiplies the optimizer cannot fold.
#[inline(never)]
pub fn payload(x: u64) -> u64 {
    let mut h = black_box(x);
    for _ in 0..PAYLOAD_ROUNDS {
        h = (h ^ (h >> 29)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    }
    h
}

/// The three trivial adapter functions of the shallow shape.
#[inline(always)]
fn first(x: u64) -> u64 {
    x.wrapping_add(1)
}
#[inline(always)]
fn keep(x: &u64) -> bool {
    x % 7 != 0
}
#[inline(always)]
fn last(x: u64) -> u64 {
    x.wrapping_mul(3)
}

/// The shallow shape: one input through three adapters into a hold, one
/// node, or with `share` two nodes and one real hop. `heavy` puts one
/// [`payload`] call in the first adapter.
pub struct Shallow {
    pub graph: Runtime,
    pub input: Input<u64>,
    pub out: Cell<u64>,
}

impl Shallow {
    pub fn new(share: bool, heavy: bool) -> Self {
        let (graph, edge) = Runtime::build(|b| {
            let (numbers, input) = b.input::<u64>();
            let mapped = numbers.map(move |x| if heavy { payload(x) } else { first(x) });
            let out = if share {
                mapped.share(b).filter(keep).map(last).hold(b, 0u64)
            } else {
                mapped.filter(keep).map(last).hold(b, 0u64)
            };
            (input, out)
        });
        let (input, out) = edge.keep();
        Shallow { graph, input, out }
    }

    /// One transaction.
    #[inline]
    pub fn send(&mut self, x: u64) {
        self.graph.send(self.input, x);
    }

    pub fn value(&self) -> u64 {
        *self.graph.sample(self.out)
    }
}

/// The shallow baseline: the same functions as plain calls into a variable.
pub struct ShallowBaseline {
    pub held: u64,
    pub heavy: bool,
}

impl ShallowBaseline {
    #[inline]
    pub fn send(&mut self, x: u64) {
        let x = if self.heavy { payload(x) } else { first(x) };
        if keep(&x) {
            self.held = last(x);
        }
    }
}

/// Inputs of the frame shape.
pub const FRAME_INPUTS: usize = 1000;

/// The frame shape: a thousand inputs per transaction into ten thousand
/// nodes, all of them affected. Per input, ten nodes: the input, its share,
/// a mapped branch, its merge with a second branch, the merge's share, a
/// hold of it, a snapshot of that hold into a second hold, a filtered node
/// and its hold, and a gated hold.
pub struct Frame {
    pub graph: Runtime,
    pub inputs: Vec<Input<u64>>,
    pub outs: Vec<Cell<u64>>,
}

impl Frame {
    pub fn new() -> Self {
        let (graph, edge) = Runtime::build(|b| {
            let (open, _open_in) = b.input_cell(true);
            let mut inputs = Vec::with_capacity(FRAME_INPUTS);
            let mut outs = Vec::with_capacity(FRAME_INPUTS * 4);
            for _ in 0..FRAME_INPUTS {
                let (x, x_in) = b.input::<u64>();
                let x = x.share(b);
                let left = x.map(|v| v + 1).share(b);
                let both = left.merge(b, x.map(|v| v * 2), |l, r| l ^ r).share(b);
                let held = both.hold(b, 0u64);
                let scaled = both.snapshot(held, |v, h| v.wrapping_add(*h)).hold(b, 0u64);
                let odd = both.filter(|v| v % 2 == 1).node(b);
                let odd = odd.hold(b, 0u64);
                let gated = x.gate(open).hold(b, 0u64);
                inputs.push(x_in);
                outs.extend([held, scaled, odd, gated]);
            }
            (inputs, outs)
        });
        let (inputs, outs) = edge.keep();
        Frame {
            graph,
            inputs,
            outs,
        }
    }

    /// One transaction: every input sends.
    pub fn frame(&mut self, k: u64) {
        let inputs = &self.inputs;
        self.graph.transaction(|tx| {
            for (i, input) in inputs.iter().enumerate() {
                tx.send(*input, k + i as u64);
            }
        });
    }

    pub fn checksum(&self) -> u64 {
        self.outs
            .iter()
            .fold(0u64, |acc, c| acc.wrapping_add(*self.graph.sample(*c)))
    }
}

impl Default for Frame {
    fn default() -> Self {
        Self::new()
    }
}

/// The frame baseline: a loop over the values doing the same work into
/// plain state.
pub struct FrameBaseline {
    pub held: Vec<[u64; 4]>,
}

impl FrameBaseline {
    pub fn new() -> Self {
        FrameBaseline {
            held: vec![[0; 4]; FRAME_INPUTS],
        }
    }

    pub fn frame(&mut self, k: u64) {
        for (i, state) in self.held.iter_mut().enumerate() {
            let x = k + i as u64;
            let both = (x + 1) ^ (x * 2);
            let before = state[0];
            state[0] = both;
            state[1] = both.wrapping_add(before);
            if both % 2 == 1 {
                state[2] = both;
            }
            state[3] = x;
        }
    }

    pub fn checksum(&self) -> u64 {
        self.held
            .iter()
            .flatten()
            .fold(0u64, |acc, v| acc.wrapping_add(*v))
    }
}

impl Default for FrameBaseline {
    fn default() -> Self {
        Self::new()
    }
}

/// Listeners on the fan-out shape.
pub const LISTENERS: usize = 64;

/// The fan-out shape: one input, shared, and [`LISTENERS`] listeners on
/// it, each adding the event to a sum. One send calls every listener, so
/// the shape measures listener dispatch, which checks each listener's
/// guard is live before its call and again after.
pub struct FanOut {
    pub graph: Runtime,
    pub input: Input<u64>,
    pub sum: Rc<StdCell<u64>>,
    pub listeners: Vec<Listener>,
}

impl FanOut {
    pub fn new() -> Self {
        let (mut graph, edge) = Runtime::build(|b| {
            let (numbers, input) = b.input::<u64>();
            let numbers: Shared<u64> = numbers.share(b);
            (input, numbers)
        });
        let (input, numbers) = edge.keep();
        let sum = Rc::new(StdCell::new(0u64));
        let listeners = (0..LISTENERS)
            .map(|_| {
                let sum = sum.clone();
                graph.listen(numbers, move |n| sum.set(sum.get().wrapping_add(n)))
            })
            .collect();
        FanOut {
            graph,
            input,
            sum,
            listeners,
        }
    }

    /// One transaction.
    #[inline]
    pub fn send(&mut self, x: u64) {
        self.graph.send(self.input, x);
    }

    pub fn sum(&self) -> u64 {
        self.sum.get()
    }
}

impl Default for FanOut {
    fn default() -> Self {
        Self::new()
    }
}

/// The fan-out baseline: the same closures, boxed, called in a loop.
pub struct FanOutBaseline {
    pub sum: Rc<StdCell<u64>>,
    pub calls: Vec<Box<dyn FnMut(u64)>>,
}

impl FanOutBaseline {
    pub fn new() -> Self {
        let sum = Rc::new(StdCell::new(0u64));
        let calls = (0..LISTENERS)
            .map(|_| {
                let sum = sum.clone();
                Box::new(move |n: u64| sum.set(sum.get().wrapping_add(n))) as Box<dyn FnMut(u64)>
            })
            .collect();
        FanOutBaseline { sum, calls }
    }

    #[inline]
    pub fn send(&mut self, x: u64) {
        for call in &mut self.calls {
            call(x);
        }
    }

    pub fn sum(&self) -> u64 {
        self.sum.get()
    }
}

impl Default for FanOutBaseline {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shallow_shapes_agree_with_their_baseline() {
        for heavy in [false, true] {
            let mut base = ShallowBaseline { held: 0, heavy };
            let mut plain = Shallow::new(false, heavy);
            let mut shared = Shallow::new(true, heavy);
            for x in 0..500 {
                base.send(x);
                plain.send(x);
                shared.send(x);
            }
            assert_eq!(plain.value(), base.held);
            assert_eq!(shared.value(), base.held);
        }
    }

    #[test]
    fn the_frame_shape_agrees_with_its_baseline() {
        let mut frame = Frame::new();
        let mut base = FrameBaseline::new();
        assert_eq!(frame.graph.live_nodes(), 2 + FRAME_INPUTS * 10);
        for k in 0..5 {
            frame.frame(k);
            base.frame(k);
        }
        assert_eq!(frame.checksum(), base.checksum());
    }

    #[test]
    fn the_fan_out_shape_agrees_with_its_baseline() {
        let mut fan_out = FanOut::new();
        let mut base = FanOutBaseline::new();
        for x in 0..500 {
            fan_out.send(x);
            base.send(x);
        }
        assert_eq!(fan_out.sum(), base.sum());
        assert_eq!(fan_out.sum(), LISTENERS as u64 * (0..500).sum::<u64>());
    }
}
