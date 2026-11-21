//! The model providers Spendyard reads documents with: the capable
//! extractors (Anthropic, OpenAI, Gemini, OpenAI-compatible) and the cheap
//! first models (JEV, Clef, SystemOne), over HTTP. Each implements the core's
//! [`spendyard_core::Extractor`] or [`spendyard_core::Decider`]; the pipeline
//! itself (`spendyard_core::understand`) is the core's.
//!
//! Kept out of the core so the core builds for WebAssembly: Spendyard Cloud
//! calls its models itself, through Workers AI and AI Gateway.

pub mod decide;
pub mod extract;
