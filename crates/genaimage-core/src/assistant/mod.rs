pub mod prompt_assist;
mod prompts;
mod service;
mod store;
pub mod suggest;
pub mod tools;
mod vision;

pub use prompts::{PromptTemplate, PromptTemplates, DEFAULT_SYSTEM_PROMPT, SYSTEM_PROMPT_TEMPLATE};
pub use service::{AssistantConfirmationDecision, AssistantStream, ConversationService};
pub use store::{
    AssistantConversation, AssistantConversationDetail, AssistantConversationListQuery,
    AssistantConversationPage, AssistantMessage, AssistantMessageRole, AssistantMessageStatus,
    AssistantToolAuditRecord, AssistantToolConfirmation, AssistantToolConfirmationStatus,
    ConversationStore, NewAssistantConfirmation, NewAssistantConversation, NewAssistantMessage,
    NewAssistantToolAudit,
};
pub use vision::{OutputAnnotation, VisionReviewService};

#[cfg(test)]
mod tests;
