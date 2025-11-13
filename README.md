# **Bough**

**Lightweight FRP rooted in Rust.**

<p align="center">
  <img src="https://raw.githubusercontent.com/bough-frp/bough/83b68048089c3a5c9f41215d3db3c2868ccb220b/bough-logo.png" width="280" alt="Bough Logo">
</p>

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

## Status

Bough is in early development while the API and performance model are
refined. Expect rapid iteration, breaking changes, and new modules as
the design evolves.

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

## Inspiration

Bough draws significant inspiration from the design philosophy and
structure of the **Sodium FRP** library, while reworking the internals
and API to better fit Rust’s model of memory, ownership, and zero-cost
composability.

---

## License

Copyright (c) 2025 Zefira Shannon

Licensed under the Apache License, Version 2.0 (the "License"); you
may not use this file except in compliance with the License.  You may
obtain a copy of the License at http://www.apache.org/licenses/LICENSE-2.0

Unless required by applicable law or agreed to in writing, software
distributed under the License is distributed on an "AS IS" BASIS,
WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or
implied.  See the License for the specific language governing
permissions and limitations under the License.
