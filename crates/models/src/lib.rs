//! The model providers Spendyard reads documents with: the capable
//! extractors (Anthropic, OpenAI, Gemini, OpenAI-compatible) and the cheap
//! first models (JEV, Clef, SystemOne), over HTTP. Each implements the core's
//! [`spendyard_core::Extractor`] or [`spendyard_core::Decider`]; the pipeline
//! itself (`spendyard_core::understand`) is the core's.
//!
//! They post through a [`extract::Transport`]: reqwest natively (the
//! `reqwest` feature, on by default), or one the host gives. With the feature
//! off it builds for WebAssembly, and Spendyard Cloud reads documents with the
//! same providers as the CLI, posting with the Worker's `fetch` to AI Gateway
//! and Workers AI.

pub mod decide;
pub mod extract;
