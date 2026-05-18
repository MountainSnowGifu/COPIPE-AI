mod context;
pub mod debug_log;
mod log;
pub(crate) mod outcome;
mod parser;
mod prompt;
pub mod rate_limiter;
mod runner;
pub mod session_store;
mod task;

pub use prompt::build_system_prompt;
pub use runner::run_agent;
pub use session_store::SessionStore;

/// 非アクション系タスク（「続き」「OK」など）かどうかを判定する。
/// LLM を呼ばずに main.rs で早期リターンするために使う。
pub fn is_clarification_only_task(task: &str) -> bool {
    task::is_non_actionable_ack(task)
        || task::is_menu_selection_without_context(task)
        || task::is_ambiguous_file_fill_request(task)
}
