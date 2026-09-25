//! `Threaded` refuses what is not `Send`, at every place a graph stores a
//! value (RFD 6): each materializer, each closer and each listener, for an
//! event, a state or a value of a type that is not `Send`, and for a
//! closure that captures one. Belt and braces: the engine can store a user
//! type only through `Accepts::erase`, so a bound left off a materializer
//! already fails the engine's own build; these fix the public surface.
//! Each example is the smallest graph that stores one `Rc`, and each fails
//! with E0277, `Rc` cannot be sent between threads safely. The module
//! exists only when doc tests are collected.
//!
//! Where a type can only reach a materializer through a token of another
//! graph, a `Local` one, the example takes that token: the refusal is at
//! compile time, before the foreign token would panic. `switch_cell`,
//! `cell_loop`, `state_loop` and their closers store nothing of their own,
//! so they need no bound, and no `Rc` cell exists in a `Threaded` graph for
//! them to reach; the closers' examples fail at the definition's
//! materializer. A listener's event is refused where its stream was made,
//! so the listener examples are captures.
//!
//! The build's return value, which the graph keeps as its edge:
//!
//! ```compile_fail,E0277
//! # use std::rc::Rc;
//! # use bough::Graph;
//! let _ = Graph::build_threaded(|_| Rc::new(0u32));
//! ```
//!
//! # Inputs and constants
//!
//! `input`, an event:
//!
//! ```compile_fail,E0277
//! # use std::rc::Rc;
//! # use bough::Graph;
//! let _ = Graph::build_threaded(|b| {
//!     let _ = b.input::<Rc<u32>>();
//! });
//! ```
//!
//! `input_coalescing`, an event and a capture:
//!
//! ```compile_fail,E0277
//! # use std::rc::Rc;
//! # use bough::Graph;
//! let _ = Graph::build_threaded(|b| {
//!     let _ = b.input_coalescing(|a: Rc<u32>, _: Rc<u32>| a);
//! });
//! ```
//!
//! ```compile_fail,E0277
//! # use std::rc::Rc;
//! # use bough::Graph;
//! let _ = Graph::build_threaded(|b| {
//!     let rc = Rc::new(1u32);
//!     let _ = b.input_coalescing(move |a: u32, c: u32| a + c + *rc);
//! });
//! ```
//!
//! `input_cell`, a value:
//!
//! ```compile_fail,E0277
//! # use std::rc::Rc;
//! # use bough::Graph;
//! let _ = Graph::build_threaded(|b| {
//!     let _ = b.input_cell(Rc::new(0u32));
//! });
//! ```
//!
//! `input_cell_coalescing`, a value and a capture:
//!
//! ```compile_fail,E0277
//! # use std::rc::Rc;
//! # use bough::Graph;
//! let _ = Graph::build_threaded(|b| {
//!     let _ = b.input_cell_coalescing(Rc::new(0u32), |a, _| a);
//! });
//! ```
//!
//! ```compile_fail,E0277
//! # use std::rc::Rc;
//! # use bough::Graph;
//! let _ = Graph::build_threaded(|b| {
//!     let rc = Rc::new(1u32);
//!     let _ = b.input_cell_coalescing(0u32, move |a, c| a + c + *rc);
//! });
//! ```
//!
//! `constant`, a value:
//!
//! ```compile_fail,E0277
//! # use std::rc::Rc;
//! # use bough::Graph;
//! let _ = Graph::build_threaded(|b| {
//!     let _ = b.constant(Rc::new(0u32));
//! });
//! ```
//!
//! # Stream materializers
//!
//! `hold`, an event and a capture:
//!
//! ```compile_fail,E0277
//! # use std::rc::Rc;
//! # use bough::{Graph, Source};
//! let _ = Graph::build_threaded(|b| {
//!     let (numbers, _numbers_in) = b.input::<u32>();
//!     let _ = numbers.map(Rc::new).hold(b, Rc::new(0));
//! });
//! ```
//!
//! ```compile_fail,E0277
//! # use std::rc::Rc;
//! # use bough::{Graph, Source};
//! let _ = Graph::build_threaded(|b| {
//!     let (numbers, _numbers_in) = b.input::<u32>();
//!     let rc = Rc::new(1u32);
//!     let _ = numbers.map(move |n| n + *rc).hold(b, 0);
//! });
//! ```
//!
//! `node`, an event and a capture:
//!
//! ```compile_fail,E0277
//! # use std::rc::Rc;
//! # use bough::{Graph, Source};
//! let _ = Graph::build_threaded(|b| {
//!     let (numbers, _numbers_in) = b.input::<u32>();
//!     let _ = numbers.map(Rc::new).node(b);
//! });
//! ```
//!
//! ```compile_fail,E0277
//! # use std::rc::Rc;
//! # use bough::{Graph, Source};
//! let _ = Graph::build_threaded(|b| {
//!     let (numbers, _numbers_in) = b.input::<u32>();
//!     let rc = Rc::new(1u32);
//!     let _ = numbers.map(move |n| n + *rc).node(b);
//! });
//! ```
//!
//! `share`, an event and a capture:
//!
//! ```compile_fail,E0277
//! # use std::rc::Rc;
//! # use bough::{Graph, Source};
//! let _ = Graph::build_threaded(|b| {
//!     let (numbers, _numbers_in) = b.input::<u32>();
//!     let _ = numbers.map(Rc::new).share(b);
//! });
//! ```
//!
//! ```compile_fail,E0277
//! # use std::rc::Rc;
//! # use bough::{Graph, Source};
//! let _ = Graph::build_threaded(|b| {
//!     let (numbers, _numbers_in) = b.input::<u32>();
//!     let rc = Rc::new(1u32);
//!     let _ = numbers.map(move |n| n + *rc).share(b);
//! });
//! ```
//!
//! `merge`, an event and a capture:
//!
//! ```compile_fail,E0277
//! # use std::rc::Rc;
//! # use bough::{Graph, Source};
//! let _ = Graph::build_threaded(|b| {
//!     let (numbers, _numbers_in) = b.input::<u32>();
//!     let (others, _others_in) = b.input::<u32>();
//!     let _ = numbers.map(Rc::new).merge(b, others.map(Rc::new), |a, _| a);
//! });
//! ```
//!
//! ```compile_fail,E0277
//! # use std::rc::Rc;
//! # use bough::{Graph, Source};
//! let _ = Graph::build_threaded(|b| {
//!     let (numbers, _numbers_in) = b.input::<u32>();
//!     let (others, _others_in) = b.input::<u32>();
//!     let rc = Rc::new(1u32);
//!     let _ = numbers.merge(b, others, move |a, c| a + c + *rc);
//! });
//! ```
//!
//! `or_else`, an event and a capture:
//!
//! ```compile_fail,E0277
//! # use std::rc::Rc;
//! # use bough::{Graph, Source};
//! let _ = Graph::build_threaded(|b| {
//!     let (numbers, _numbers_in) = b.input::<u32>();
//!     let (others, _others_in) = b.input::<u32>();
//!     let _ = numbers.map(Rc::new).or_else(b, others.map(Rc::new));
//! });
//! ```
//!
//! ```compile_fail,E0277
//! # use std::rc::Rc;
//! # use bough::{Graph, Source};
//! let _ = Graph::build_threaded(|b| {
//!     let (numbers, _numbers_in) = b.input::<u32>();
//!     let (others, _others_in) = b.input::<u32>();
//!     let rc = Rc::new(1u32);
//!     let _ = numbers.map(move |n| n + *rc).or_else(b, others);
//! });
//! ```
//!
//! `accumulate`, a state and a capture:
//!
//! ```compile_fail,E0277
//! # use std::rc::Rc;
//! # use bough::{Graph, Source};
//! let _ = Graph::build_threaded(|b| {
//!     let (numbers, _numbers_in) = b.input::<u32>();
//!     let _ = numbers.accumulate(b, Rc::new(0u32), |n, s| Rc::new(n + **s));
//! });
//! ```
//!
//! ```compile_fail,E0277
//! # use std::rc::Rc;
//! # use bough::{Graph, Source};
//! let _ = Graph::build_threaded(|b| {
//!     let (numbers, _numbers_in) = b.input::<u32>();
//!     let rc = Rc::new(1u32);
//!     let _ = numbers.accumulate(b, 0u32, move |n, s| n + s + *rc);
//! });
//! ```
//!
//! `accumulate_mut`, an event, a state and a capture:
//!
//! ```compile_fail,E0277
//! # use std::rc::Rc;
//! # use bough::{Graph, Source};
//! let _ = Graph::build_threaded(|b| {
//!     let (numbers, _numbers_in) = b.input::<u32>();
//!     let _ = numbers
//!         .map(Rc::new)
//!         .accumulate_mut(b, 0u32, |n, s: &mut u32| *s += *n);
//! });
//! ```
//!
//! ```compile_fail,E0277
//! # use std::rc::Rc;
//! # use bough::{Graph, Source};
//! let _ = Graph::build_threaded(|b| {
//!     let (numbers, _numbers_in) = b.input::<u32>();
//!     let _ = numbers.accumulate_mut(b, Rc::new(0u32), |n, s: &mut Rc<u32>| *s = Rc::new(n));
//! });
//! ```
//!
//! ```compile_fail,E0277
//! # use std::rc::Rc;
//! # use bough::{Graph, Source};
//! let _ = Graph::build_threaded(|b| {
//!     let (numbers, _numbers_in) = b.input::<u32>();
//!     let rc = Rc::new(1u32);
//!     let _ = numbers.accumulate_mut(b, 0u32, move |n, s: &mut u32| *s += n + *rc);
//! });
//! ```
//!
//! `scan`, an event, a state and a capture:
//!
//! ```compile_fail,E0277
//! # use std::rc::Rc;
//! # use bough::{Graph, Source};
//! let _ = Graph::build_threaded(|b| {
//!     let (numbers, _numbers_in) = b.input::<u32>();
//!     let _ = numbers.scan(b, 0u32, |n, s| (Rc::new(n), s + 1));
//! });
//! ```
//!
//! ```compile_fail,E0277
//! # use std::rc::Rc;
//! # use bough::{Graph, Source};
//! let _ = Graph::build_threaded(|b| {
//!     let (numbers, _numbers_in) = b.input::<u32>();
//!     let _ = numbers.scan(b, Rc::new(0u32), |n, s| (n, Rc::new(n + **s)));
//! });
//! ```
//!
//! ```compile_fail,E0277
//! # use std::rc::Rc;
//! # use bough::{Graph, Source};
//! let _ = Graph::build_threaded(|b| {
//!     let (numbers, _numbers_in) = b.input::<u32>();
//!     let rc = Rc::new(1u32);
//!     let _ = numbers.scan(b, 0u32, move |n, s| (n + *rc, s + 1));
//! });
//! ```
//!
//! `split`, an element and a capture:
//!
//! ```compile_fail,E0277
//! # use std::rc::Rc;
//! # use bough::{Graph, Source};
//! let _ = Graph::build_threaded(|b| {
//!     let (numbers, _numbers_in) = b.input::<u32>();
//!     let _ = numbers.map(|n| vec![Rc::new(n)]).split(b);
//! });
//! ```
//!
//! ```compile_fail,E0277
//! # use std::rc::Rc;
//! # use bough::{Graph, Source};
//! let _ = Graph::build_threaded(|b| {
//!     let (numbers, _numbers_in) = b.input::<u32>();
//!     let rc = Rc::new(1u32);
//!     let _ = numbers.map(move |n| vec![n + *rc]).split(b);
//! });
//! ```
//!
//! `defer`, an event and a capture:
//!
//! ```compile_fail,E0277
//! # use std::rc::Rc;
//! # use bough::{Graph, Source};
//! let _ = Graph::build_threaded(|b| {
//!     let (numbers, _numbers_in) = b.input::<u32>();
//!     let _ = numbers.map(Rc::new).defer(b);
//! });
//! ```
//!
//! ```compile_fail,E0277
//! # use std::rc::Rc;
//! # use bough::{Graph, Source};
//! let _ = Graph::build_threaded(|b| {
//!     let (numbers, _numbers_in) = b.input::<u32>();
//!     let rc = Rc::new(1u32);
//!     let _ = numbers.map(move |n| n + *rc).defer(b);
//! });
//! ```
//!
//! `construct`, an event and a capture:
//!
//! ```compile_fail,E0277
//! # use std::rc::Rc;
//! # use bough::{Graph, Source};
//! let _ = Graph::build_threaded(|b| {
//!     let (numbers, _numbers_in) = b.input::<u32>();
//!     let _ = numbers.construct(b, |_, n| Rc::new(n));
//! });
//! ```
//!
//! ```compile_fail,E0277
//! # use std::rc::Rc;
//! # use bough::{Graph, Source};
//! let _ = Graph::build_threaded(|b| {
//!     let (numbers, _numbers_in) = b.input::<u32>();
//!     let rc = Rc::new(1u32);
//!     let _ = numbers.construct(b, move |b, n| b.constant(n + *rc));
//! });
//! ```
//!
//! # Cell materializers
//!
//! `map_cell`, a value and a capture, and over a `State`:
//!
//! ```compile_fail,E0277
//! # use std::rc::Rc;
//! # use bough::Graph;
//! let _ = Graph::build_threaded(|b| {
//!     let (level, _level_in) = b.input_cell(1u32);
//!     let _ = level.map_cell(b, |l| Rc::new(*l));
//! });
//! ```
//!
//! ```compile_fail,E0277
//! # use std::rc::Rc;
//! # use bough::Graph;
//! let _ = Graph::build_threaded(|b| {
//!     let (level, _level_in) = b.input_cell(1u32);
//!     let rc = Rc::new(1u32);
//!     let _ = level.map_cell(b, move |l| l + *rc);
//! });
//! ```
//!
//! ```compile_fail,E0277
//! # use std::rc::Rc;
//! # use bough::{Graph, Source};
//! let _ = Graph::build_threaded(|b| {
//!     let (numbers, _numbers_in) = b.input::<u32>();
//!     let total = numbers.accumulate_mut(b, 0u32, |n, s: &mut u32| *s += n);
//!     let _ = total.map_cell(b, |t| Rc::new(*t));
//! });
//! ```
//!
//! `lift`, a value and a capture:
//!
//! ```compile_fail,E0277
//! # use std::rc::Rc;
//! # use bough::{Graph, Lift};
//! let _ = Graph::build_threaded(|b| {
//!     let (level, _level_in) = b.input_cell(1u32);
//!     let _ = (level, level).lift(b, |a, c| Rc::new(a + c));
//! });
//! ```
//!
//! ```compile_fail,E0277
//! # use std::rc::Rc;
//! # use bough::{Graph, Lift};
//! let _ = Graph::build_threaded(|b| {
//!     let (level, _level_in) = b.input_cell(1u32);
//!     let rc = Rc::new(1u32);
//!     let _ = (level, level).lift(b, move |a, c| a + c + *rc);
//! });
//! ```
//!
//! `steps` and `steps_with_current`, over a `Local` graph's cell of `Rc`s:
//!
//! ```compile_fail,E0277
//! # use std::rc::Rc;
//! # use bough::Graph;
//! let (_local, shared) = Graph::build(|b| b.constant(Rc::new(0u32)));
//! let _ = Graph::build_threaded(move |b| {
//!     let _ = shared.steps(b);
//! });
//! ```
//!
//! ```compile_fail,E0277
//! # use std::rc::Rc;
//! # use bough::Graph;
//! let (_local, shared) = Graph::build(|b| b.constant(Rc::new(0u32)));
//! let _ = Graph::build_threaded(move |b| {
//!     let _ = shared.steps_with_current(b);
//! });
//! ```
//!
//! `switch_stream`, over a `Local` graph's stream of `Rc`s:
//!
//! ```compile_fail,E0277
//! # use std::rc::Rc;
//! # use bough::Graph;
//! let (_local, shared) = Graph::build(|b| b.input::<Rc<u32>>().0);
//! let _ = Graph::build_threaded(move |b| {
//!     let _ = b.constant(shared).switch_stream(b);
//! });
//! ```
//!
//! # Closers
//!
//! `StreamLoop::close`, an event and a capture:
//!
//! ```compile_fail,E0277
//! # use std::rc::Rc;
//! # use bough::{Graph, Source};
//! let _ = Graph::build_threaded(|b| {
//!     let (numbers, _numbers_in) = b.input::<u32>();
//!     let (_forward, forward_loop) = b.stream_loop::<Rc<u32>>();
//!     forward_loop.close(b, numbers.map(Rc::new));
//! });
//! ```
//!
//! ```compile_fail,E0277
//! # use std::rc::Rc;
//! # use bough::{Graph, Source};
//! let _ = Graph::build_threaded(|b| {
//!     let (numbers, _numbers_in) = b.input::<u32>();
//!     let (_forward, forward_loop) = b.stream_loop::<u32>();
//!     let rc = Rc::new(1u32);
//!     forward_loop.close(b, numbers.map(move |n| n + *rc));
//! });
//! ```
//!
//! `CellLoop::close` and `StateLoop::close`, refused at the definition:
//!
//! ```compile_fail,E0277
//! # use std::rc::Rc;
//! # use bough::{Graph, Source};
//! let _ = Graph::build_threaded(|b| {
//!     let (numbers, _numbers_in) = b.input::<u32>();
//!     let (_forward, forward_loop) = b.cell_loop::<Rc<u32>>();
//!     let definition = numbers.map(Rc::new).hold(b, Rc::new(0));
//!     forward_loop.close(b, definition);
//! });
//! ```
//!
//! ```compile_fail,E0277
//! # use std::rc::Rc;
//! # use bough::{Graph, Source};
//! let _ = Graph::build_threaded(|b| {
//!     let (numbers, _numbers_in) = b.input::<u32>();
//!     let (_forward, forward_loop) = b.state_loop::<Rc<u32>>();
//!     let definition =
//!         numbers.accumulate_mut(b, Rc::new(0u32), |n, s: &mut Rc<u32>| *s = Rc::new(n));
//!     forward_loop.close(b, definition);
//! });
//! ```
//!
//! # Listeners
//!
//! `listen` and `try_listen`, captures:
//!
//! ```compile_fail,E0277
//! # use std::rc::Rc;
//! # use bough::{Graph, Source};
//! let (mut graph, numbers) = Graph::build_threaded(|b| b.input::<u32>().0);
//! let rc = Rc::new(1u32);
//! let _ = graph.listen(numbers, move |n| assert!(n < *rc));
//! ```
//!
//! ```compile_fail,E0277
//! # use std::rc::Rc;
//! # use bough::{Graph, Source};
//! let (mut graph, numbers) = Graph::build_threaded(|b| b.input::<u32>().0);
//! let rc = Rc::new(1u32);
//! let _ = graph.try_listen(numbers, move |n| assert!(n < *rc));
//! ```
//!
//! `listen_cell` and `try_listen_cell`, captures:
//!
//! ```compile_fail,E0277
//! # use std::rc::Rc;
//! # use bough::Graph;
//! let (mut graph, level) = Graph::build_threaded(|b| b.input_cell(1u32).0);
//! let rc = Rc::new(1u32);
//! let _ = graph.listen_cell(level, move |l| assert!(*l < *rc));
//! ```
//!
//! ```compile_fail,E0277
//! # use std::rc::Rc;
//! # use bough::Graph;
//! let (mut graph, level) = Graph::build_threaded(|b| b.input_cell(1u32).0);
//! let rc = Rc::new(1u32);
//! let _ = graph.try_listen_cell(level, move |l| assert!(*l < *rc));
//! ```
//!
//! `listen_steps` and `try_listen_steps`, captures:
//!
//! ```compile_fail,E0277
//! # use std::rc::Rc;
//! # use bough::Graph;
//! let (mut graph, level) = Graph::build_threaded(|b| b.input_cell(1u32).0);
//! let rc = Rc::new(1u32);
//! let _ = graph.listen_steps(level, move |l| assert!(*l < *rc));
//! ```
//!
//! ```compile_fail,E0277
//! # use std::rc::Rc;
//! # use bough::Graph;
//! let (mut graph, level) = Graph::build_threaded(|b| b.input_cell(1u32).0);
//! let rc = Rc::new(1u32);
//! let _ = graph.try_listen_steps(level, move |l| assert!(*l < *rc));
//! ```
