//! Los cinco contratos, mitad de entrada.

pub mod error;
pub mod request;
pub mod response;
pub mod vocab;

pub use error::BrainError;
pub use request::{
    BrainRequest, Message, ProjectContext, RequestIssue, ToolInfo, ToolSet, TrustState,
    VerifyCommands,
};
pub use response::{
    BrainResult, DecisionTrace, Evidence, Output, OutputStatus, TaskMetrics, ToolCall,
};
pub use vocab::{
    ApprovalLevel, Confidence, DecisionSource, ExecutionPolicy, ExecutionTarget, Intent, KeepAlive,
    Level, Mode, ModelId, ModelTarget, OutputContract, Profile, ProviderId, Risk, Signals,
    ThinkingLevel, ToolId, VerificationMode,
};
