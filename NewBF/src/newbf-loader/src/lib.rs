//! `newbf-loader` — the NewBF module graph and hot-swap loader.
//!
//! Tracks the module/workspace graph with source-stamp invalidation and
//! per-definition dirty state, and drives incremental recompilation. Owns
//! the generation discipline behind hot code swapping: a recompiled
//! method is installed under a new generation and the old body retires
//! once no live frame can reach it. With no GC to relocate references,
//! object identity is simpler — but code retirement still matters.
//!
//! Lands across SPRINTS.md (incremental from Sprint 05; hot swap in the
//! late sprints). The generation model is lifted from NewCormanLisp /
//! NewCP.
