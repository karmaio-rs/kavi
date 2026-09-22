# AGENTS.md - Developer Guidelines for Kavi

## Project Overview

Kavi is a modern full stack web framework built specifically for the Karmaio completion-based runtime.
It is not a micro-framework in the Axum/Express sense. The intended comparison is Rails and Laravel: conventions, generators, an ORM, views, mail, jobs, and a CLI that bootstraps an application.

Kavi is intentionally tightly coupled to Karmaio. Do not introduce runtime abstraction layers unless there is a strong architectural reason.

## Project Layout

```
kavi/
├── Cargo.toml                 # workspace root
├── crates/
│   ├── kavi/                  # library (the framework)
│   ├── kavi-macros/           # proc macros
├── tools/
│   └── xtask/                 # maintainer tasks only
├── examples/                  # small apps that consume crates/kavi
├── templates/                 # `kavi new` application skeletons
└── guides/
```

## Package Management, Build, Lint, and Test Commands

This is a multi package cargo workspace project. All the standard rust/cargo commands and patterns will work.

## Code Style Guidelines

### Naming Conventions

- **Traits**: `AsyncRead`, `AsyncWrite`, `Driver` - PascalCase, descriptive
- **Structs/Enums**: `Task<S>`, `Op<T>` - PascalCase
- **Functions/Methods**: `from_raw`, `schedule`, `run` - snake_case
- **Variables**: `task_ptr`, `raw`, `buf` - snake_case
- **Constants**: SCREAMING_SNAKE_CASE
- **Generic Type Parameters**: Single uppercase letter `T`, `S`, `F`, `B`

In case the above guidelines don't cover something, default to standard rust conventions.

### Visibility

- Use `pub(crate)` for module-level public items
- Use `pub` only for truly public API
- Use `pub(super)` for parent-visible items
- Keep implementation details private

### Async/Await

- Use `async fn` for async functions instead of `impl Future`
- Use `.await` directly without additional wrapping

### Error Handling

- Use `std::io::Result<T>` for std I/O operations
- Use `(std::io::Result<T>, B)` tuples for buffer-based I/O operations (`BufResult`)
- Propagate errors with `?` operator
- Document error conditions in doc comments

### Unsafe Code

- Do not use `unsafe` code here. It is forbidden in this project

### Traits and trait implementations

- Add comments to public traits and functions explaining purpose and contract
- Use `#[inline]` on simple trait implementations

### Documentation

- Use complete sentences in documentation
- Document `unsafe` preconditions

### Testing

- Use `#[test]` attribute for unit tests
- Group tests in the same module or in `tests/` directory
- Use descriptive test names: `test_name_describes_behavior`

### Module Organization

- Use `pub mod` for public modules
- Use `pub(crate) mod` for crate-internal modules
- Use `mod` for private modules
