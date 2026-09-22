# Bough

A functional reactive programming library that implements the Sodium
denotational semantics and keeps building FRP logic and driving it from I/O
in two separate worlds. This glossary is the project's language; the design
behind each term is in the RFDs at <https://github.com/bough-frp/rfd>.

## Language

### Time

**Instant**:
A point in the semantics' time. Everything within one instant is
simultaneous, and a stream has at most one occurrence per instant.
_Avoid_: tick, time step, frame

**Transaction**:
One instant as the engine runs it, from the sends that open it to the
listeners that close it. The initial build is transaction zero.
_Avoid_: batch, tick

**Child transaction**:
A transaction that `split` or `defer` schedules to run after its parent and
before the next external instant.
_Avoid_: post-transaction, sub-transaction

**Occurrence**:
A stream's value at one instant. A stream *fires* when it has one.
_Avoid_: event, firing (as a noun), message

**Step**:
A change of a cell's value from one instant to the next, including a change
to an equal value.
_Avoid_: update, change

### Streams and cells

**Stream**:
A stream with exactly one consumer, whose occurrences move through by value.
Every constructor takes it by value, so using it twice is a compile error.
_Avoid_: event stream, linear token

**Shared stream**:
A stream with any number of consumers, each of which clones the occurrence.
The only way to give a stream more than one consumer.
_Avoid_: broadcast stream, fan-out stream

**Linear**:
Having exactly one consumer. Streams and chains are linear.
_Avoid_: affine, single-use, unique

**Fan-out**:
Giving a stream more than one consumer, which is always explicit, through
`share`.
_Avoid_: splitting (that is `split`), broadcasting

**Cell**:
A value that exists at every instant. Cell values are read by reference and
are never cloned by the engine.
_Avoid_: behavior, signal, property, variable

**Hold**:
A cell that keeps the latest occurrence of a stream, starting from an
initial value. Holds and accumulators are the stateful cells.
_Avoid_: register, latch, state cell

**Accumulator**:
A cell whose value is folded from a stream's occurrences, either by
returning a new state or by mutating the state in place.
_Avoid_: reducer, fold cell

**Input**:
A stream or cell driven from outside the graph, and the token I/O code sends
with.
_Avoid_: sink, source, port, event sink

**Source**:
The role of anything that yields occurrences: a stream, a shared stream, or a
chain. An input is a source; a source is not necessarily an input.
_Avoid_: producer, emitter

### Building

**Token**:
The name of a node that the four token types carry: `Stream`, `Shared`,
`Cell`, `Input`. A token has no method that creates a node without a build
context.
_Avoid_: handle, reference, id

**Node**:
Anything in the graph with an identity of its own, created by a
materializer.
_Avoid_: vertex, operator

**Chain**:
A stream with stages applied and no node yet. The first materializer
consumes it, and the stages fuse into that node.
_Avoid_: pipeline, builder, lazy stream

**Materializer**:
An operation that creates one node, from a chain or from a cell, and takes
the build context to do it.
_Avoid_: terminal operation, consumer, sink

**Build context**:
The context every node-creating operation requires. It exists inside the
build closure and inside construct closures, and nowhere else.
_Avoid_: builder, transaction (Sodium's word for it)

**Construct**:
Creating nodes during a transaction, from a construct closure. The only way
logic is added after build.
_Avoid_: dynamic construction, runtime wiring, late binding

**Scope**:
The extent of one build context: the initial build, or one run of a
construct closure. A loop closes in the scope that declared it.
_Avoid_: session, phase

**Loop**:
A cycle in the graph, declared with a forward token and closed later with a
definition. Every path around a loop passes through a hold, an accumulator,
a `split` or a `defer`.
_Avoid_: cycle (for the construct; a cycle is what a loop makes legal), recursion

**Forward token**:
The token a loop hands out before its definition exists.
_Avoid_: placeholder, forward declaration

**Closer**:
The value that defines a loop, consumed by `close`.

### Driving

**Driver**:
Whoever owns a graph and runs its transactions.
_Avoid_: runtime, executor, owner

**Listener**:
An I/O callback attached to a node, run after commit with no graph access;
also the RAII handle that keeps it attached.
_Avoid_: observer, subscriber, callback (for the attachment)

**Remote**:
A `Send + Clone` handle for sending into a graph from any thread. Each
remote send is its own transaction, run when the driver pumps.
_Avoid_: sender, proxy, channel

**Pump**:
Running every pending remote send, each as its own transaction, in arrival
order.
_Avoid_: poll, drain, flush

**Mode**:
Whether a graph is `Local` or `Threaded`: whether what it stores must be
`Send`, and whether the graph itself is.
_Avoid_: flavor, threading model

### Memory

**Root**:
Something that keeps a node alive: the value the build closure returned, a
live listener, or a live `Root` handle taken with `root`.
_Avoid_: anchor, pin, owner

**Stale**:
Of a token: its node has been collected.
_Avoid_: dangling, dead, expired

**Foreign**:
Of a token: it belongs to another graph.
_Avoid_: mismatched, alien

**Poisoned**:
Of a graph: a panic escaped `send`, and every later call fails.
_Avoid_: broken, corrupted, tainted

**Collection**:
Reclaiming the nodes no root reaches. Never runs inside a transaction.
_Avoid_: GC in prose, sweeping, cleanup

### Testing and performance

**The semantics**:
The Sodium denotational semantics, version 1.1, as the executable Haskell in
the Sodium repository. What the engine is held to.
_Avoid_: the spec, the reference implementation

**Oracle**:
The semantics ported to Rust as lists of time-stamped values, which the
engine is property-tested against.
_Avoid_: reference model, golden model

**Shape**:
One of the three benchmark workloads, UI, frame and shallow, each with a
hand-written imperative baseline.
_Avoid_: scenario, benchmark case

**Bar**:
The performance target: within a factor of three of the baseline on
realistic per-node payloads.
_Avoid_: budget, goal, SLA
