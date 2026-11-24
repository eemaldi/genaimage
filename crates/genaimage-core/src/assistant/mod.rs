mod prompts;
mod service;
mod store;

pub use prompts::{PromptTemplate, PromptTemplates, DEFAULT_SYSTEM_PROMPT, SYSTEM_PROMPT_TEMPLATE};
pub use service::{AssistantStream, ConversationService};
pub use store::{
    AssistantConversation, AssistantConversationDetail, AssistantConversationListQuery,
    AssistantConversationPage, AssistantMessage, AssistantMessageRole, AssistantMessageStatus,
    ConversationStore, NewAssistantConversation, NewAssistantMessage,
};

#[cfg(test)]
mod tests;
