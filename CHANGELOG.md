# Changelog

## [Unreleased]

### Added

- Defined and frozen History / Trace semantic guarantees (docs/api-spec.md).
- Workspace layout: `bpm-core`, `bpm-storage`, `bpm-runtime`, `bpm-adapter-memory`, `bpm-server-rest` crates (refactor.md).
- Async storage traits in `bpm-storage`; memory adapter and unified `MemoryRepo` in `bpm-adapter-memory`.
- Async event pump and handlers in `bpm-runtime`; REST server in `bpm-server-rest`.

### Changed

- Root package re-exports workspace crates; main binary moved to `bpm-server-rest` (`cargo run -p bpm-server-rest`).
