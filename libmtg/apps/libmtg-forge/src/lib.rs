pub mod agent;
pub mod jev;
pub mod matchup;
pub mod profile;
pub mod search;
pub mod strategy;

pub use matchup::{audit, run_matchup, run_paired_matchup, DeckAudit, MatchupStats, PairedMatchupStats, PartialSupport};
pub use profile::{compare_openings, simulate_openings, CompareStats, OpeningStats};

pub use search::{compare_search_smoke, evaluate_forge_state, generate_training_jsonl, run_search_smoke_once, ForgeSearchConfig, ForgeSearchStrategy, PilotRunSummary, SearchSmokeComparison, TrainingGenerationStats};

pub use agent::{
    agent_action_from_legal, build_agent_observation, legal_action_id, legal_action_semantic,
    run_agent_smoke, run_agent_smoke_with_agent, AgentAction, AgentActionKind, AgentAnnouncementDecision,
    AgentAnnouncementObservation, AgentBackedStrategy, AgentCardRef, AgentDecision,
    AgentMana, AgentObservation, AgentPermanent, AgentSmokeSummary, AgentWishChoice,
    AgentWishObservation, ForgeAgent, HeuristicForgeAgent, InvalidForgeAgent,
    PassForgeAgent, AGENT_OBSERVATION_SCHEMA_VERSION,
};


pub use jev::{
    compare_jev_to_search, jev_check_from_env, JevCheckSummary, JevCompareSummary, JevConfig,
    JevDisagreement, JevForgeAgent, JevMetric,
};
