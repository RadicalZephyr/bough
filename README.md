# **Portia**

**Lightweight FRP, spun from Rust.**

## Overview

**Portia** is a modern, idiomatic Rust implementation of **Functional Reactive Programming
(FRP)** - built to be lightweight, expressive, and highly optimized by the compiler.

It implements [**Sodium**'s][na] [denotational semantics][na-sem], but the API and core
engine have been designed from the ground up to leverage Rust’s strengths:

* Leveraging Rust's ownership and borrowing to reduce the need for defensive cloning
* Ergonomic API with an implementation as close to zero-cost as possible

Portia aims to provide a **feature-complete FRP toolkit** while embracing Rust’s
ownership, lifetimes, and strong typing to create reactive systems that feel natural in
Rust projects.

If you’ve ever wanted FRP that feels more like Rust and less like a port, this library is
for you.

[na]: https://github.com/sodiumFRP/sodium
[na-sem]: https://github.com/SodiumFRP/sodium/tree/master/denotational

## Status

Portia v0.0.1 is an initial spike built entirely by an LLM agent to prove the interface
design is buildable and can be held to the core semantics. It is usable todaay, the code
works, nothing is `todo!()`.

Here's how the spike is checked, where each step covers what the one before leaves open:

* **The tests pin the behavior.** About half the code is tests.
* **Mutation testing shows the tests would catch a change in it.**  Hand-made mutations of
  the engine and the collector each fail at least one test.
* **Differential tests show it's the right behavior.** Random and fixed programs run
  against Sodium's denotational semantics, executed by GHC as an oracle, and the engine
  has to agree with it.

0.0.1 is a frozen snapshot, so don't expect patch releases. The next release is 0.1.0,
written by hand, and it aims to keep this API close.  If you try it, I'd like to hear how
it went: bug reports and API feedback go in [GitHub
issues](https://github.com/portia-rs/portia/issues), and they'll shape 0.1.

## Usage

Add Portia to your project using Cargo:

```toml
[dependencies]
portia = "0.0.1"
```

Then import the prelude and build a `Runtime`:


```rust
use portia::prelude::*;

fn main() -> std::io::Result<()> {
    let mut rt = Runtime::local();
    let _ = rt.build(|b| {
        <FRP code goes here>
    });
}
```

## Example

Read lines from stdin and keep a running word count and the longest line so far, printing
both after every line:

```rust
use portia::prelude::*;

fn main() -> std::io::Result<()> {
    let mut rt = Runtime::local();
    let (lines_in, summary) = rt.build(|b| {
        let (lines, lines_in) = b.input::<String>();
        let lines = lines.share(b); // two consumers below
        let words = lines.accumulate_mut(b, 0_usize, |line, n: &mut usize| {
            *n += line.split_whitespace().count();
        });
        let longest = lines.accumulate_mut(b, String::new(), |line, best: &mut String| {
            if line.chars().count() > best.chars().count() {
                *best = line;
            }
        });
        let summary = (words, longest).lift(b, |n, l| format!("{n} words, longest: {l:?}"));
        (lines_in, summary)
    });

    // Fires once now, with the initial values, then again on every step.
    let _listener = rt.listen_cell(summary, |s| println!("{s}"));
    for line in std::io::stdin().lines() {
        rt.send(lines_in, line?);
    }
    Ok(())
}
```

To quickly run this example, clone this repository, and run `cargo run --example word_count_mut`.

## Design

The design decisions behind Portia are recorded as RFDs at
<https://github.com/portia-rs/rfd>: the guiding principles and standing policies, the
separation of building FRP from driving it, the memory model, the value model, the
transaction protocol, and the I/O edge.

## Inspiration

The ideas behind Portia came out of my work on the [Rust port][na-rust] of [Sodium][na]. I
was deeply inspired by the vision that the Sodium authors articulate in [Functional
Reactive Programming][frp-book] and I think the core concept of having a small but
approachable FRP library that looks roughly the same in all the languages it is ported to
has a place. The downside is that for languages that are further from the semantics of the
original version of the library, the port feels substantially less idiomatic. Ultimately,
I realized that in order to make an FRP library that felt truly idiomatic to Rust, the API
and implementation needed to be re-thought from the ground up with Rust in mind. So,
while Portia implements the denotational semantics of Sodium, the actual code is a green
field rewrite.

[na-rust]: https://crates.io/crates/sodium-rust
[frp-book]: https://www.manning.com/books/functional-reactive-programming

## License

Copyright (c) 2025–2026 Zefira Shannon

Licensed under the Apache License, Version 2.0 (the "License"); you
may not use this file except in compliance with the License.  You may
obtain a copy of the License at http://www.apache.org/licenses/LICENSE-2.0

Unless required by applicable law or agreed to in writing, software
distributed under the License is distributed on an "AS IS" BASIS,
WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or
implied.  See the License for the specific language governing
permissions and limitations under the License.
