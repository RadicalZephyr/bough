# **Bough**

**Lightweight FRP rooted in Rust.**

<p align="center">
  <img src="https://raw.githubusercontent.com/bough-frp/bough/83b68048089c3a5c9f41215d3db3c2868ccb220b/bough-logo.png" width="280" alt="Bough Logo">
</p>

---

## Status

Bough is in early development while the API and performance model are
refined. Expect rapid iteration, breaking changes, and new modules as
the design evolves.

---

## Overview

**Bough** is a modern, idiomatic Rust implementation of **Functional
Reactive Programming (FRP)** - built to be lightweight, expressive,
and highly optimized by the compiler.

It’s heavily inspired by **Sodium**, but reimagined with Rust’s
strengths in mind:

* Clear distinction between **shared nodes** and **intermediate combinators**
* An iterator-like API designed to reduce overhead
* Zero-cost abstractions where possible
* A focus on predictable behavior, clarity, and ergonomics

Bough aims to provide a **feature-complete FRP toolkit** while
embracing Rust’s ownership, lifetimes, and strong typing to create
reactive systems that feel natural in Rust projects.

If you’ve ever wanted FRP that feels more like Rust and less like a
port, this library is for you.

---

## Installation

Add Bough to your project using Cargo:

```toml
[dependencies]
bough = "0.1"
```

---

## Example (Coming Soon)

As the API stabilizes, this section will include examples showing
stream creation, behavior propagation, and composing reactive graphs
using Bough’s iterator-like combinators.

---

## Design

The design decisions behind Bough are recorded as RFDs at
<https://github.com/bough-frp/rfd>: the guiding principles and standing
policies, the separation of building FRP from driving it, the memory
model, the value model, the transaction protocol, and the I/O edge.

---

## Inspiration

Bough draws significant inspiration from the design philosophy and
structure of the **Sodium FRP** library, while reworking the internals
and API to better fit Rust’s model of memory, ownership, and zero-cost
composability.

---

## License

Copyright (c) 2025 Zefira Shannon

This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.
