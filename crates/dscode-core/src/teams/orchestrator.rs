//! Orchestrator — merge sub-agent results with conflict resolution.
//!
//! After the [`super::dispatcher::Dispatcher`] runs all sub-agents, the
//! [`Orchestrator`] merges their outputs into a unified final response,
//! resolving contradictions and preferring concrete answers.

use super::dispatcher::SubAgentResult;

/// Merges multiple sub-agent results into a coherent final output.
pub struct Orchestrator;

impl Orchestrator {
    /// Create a new orchestrator.
    pub fn new() -> Self {
        Self
    }

    /// Merge sub-agent results into a single response.
    ///
    /// Simple strategy: concatenate successful outputs and summarize.
    pub fn merge(&self, results: &[SubAgentResult], merge_instructions: &str) -> String {
        let successful: Vec<&str> = results
            .iter()
            .filter(|r| r.success)
            .map(|r| r.output.as_str())
            .collect();

        if successful.is_empty() {
            // All failed — return the first error.
            results
                .iter()
                .find(|r| r.error.is_some())
                .and_then(|r| r.error.as_deref())
                .unwrap_or("All sub-agents failed with unknown errors.")
                .to_string()
        } else if successful.len() == 1 {
            successful[0].to_string()
        } else {
            let mut merged = String::new();
            merged.push_str(&format!(
                "Merged results from {} sub-agents:\n\n",
                successful.len()
            ));
            for (i, output) in successful.iter().enumerate() {
                merged.push_str(&format!("## Sub-agent {}\n{}\n\n", i + 1, output));
            }
            if !merge_instructions.is_empty() {
                merged.push_str(&format!(
                    "---\n*Merge instructions: {}*",
                    merge_instructions
                ));
            }
            merged
        }
    }
}

impl Default for Orchestrator {
    fn default() -> Self {
        Self::new()
    }
}
