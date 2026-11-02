pub mod assessment;
pub mod capabilities;
pub mod chatgpt;
pub mod config;
pub mod context;
pub mod copilot;
pub mod decisions;
pub mod elicitation;
pub mod engine;
pub mod history;
pub mod management;
pub mod mcp;
pub mod media;
pub mod migrate;
pub mod provider;
pub mod rpc;
pub mod server;
pub mod store;
pub mod tools;
pub mod watches;

#[cfg(windows)]
pub mod windows_command;
