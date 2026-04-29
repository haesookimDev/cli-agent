//! Map a node's `AgentRole` to a concrete Virtual Dev Team persona name.
//!
//! Same role can have multiple personas registered (e.g. Coder = Minho /
//! Yuna / Seojin). The router scores candidates against the node's
//! `instructions` using keyword overlap with each persona's `expertise`
//! and `capabilities`, then breaks ties via per-role round-robin so a
//! pool with no signal still distributes work evenly.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::agents::AgentRegistry;
use crate::types::AgentRole;

pub struct PersonaRouter {
    registry: AgentRegistry,
    round_robin: Mutex<HashMap<AgentRole, usize>>,
}

impl PersonaRouter {
    pub fn new(registry: AgentRegistry) -> Self {
        Self {
            registry,
            round_robin: Mutex::new(HashMap::new()),
        }
    }

    /// Pick a persona for `role`. `pool`, if given, restricts candidates to
    /// names in that list (intersected with registered personas of the
    /// matching role). `instructions` is the node's instruction text;
    /// keyword overlap with persona expertise/capabilities is used as a
    /// preference signal. Returns `None` when no persona of that role is
    /// registered, in which case the caller should fall back to the
    /// role's default agent.
    pub fn pick_for_role(
        &self,
        role: AgentRole,
        pool: Option<&[String]>,
        instructions: &str,
    ) -> Option<String> {
        let role_candidates = self.registry.personas_for_role(role);
        let candidates: Vec<String> = match pool {
            Some(p) => p
                .iter()
                .filter(|n| role_candidates.iter().any(|r| r == *n))
                .cloned()
                .collect(),
            None => role_candidates,
        };

        if candidates.is_empty() {
            return None;
        }
        if candidates.len() == 1 {
            return Some(candidates[0].clone());
        }

        let instr_tokens = tokenize(instructions);

        let scored: Vec<(usize, String)> = candidates
            .iter()
            .map(|name| {
                let score = self
                    .registry
                    .persona_definition(name)
                    .map(|def| score_persona(&instr_tokens, &def))
                    .unwrap_or(0);
                (score, name.clone())
            })
            .collect();

        let max_score = scored.iter().map(|(s, _)| *s).max().unwrap_or(0);
        let tied: Vec<String> = if max_score == 0 {
            candidates.clone()
        } else {
            scored
                .iter()
                .filter(|(s, _)| *s == max_score)
                .map(|(_, n)| n.clone())
                .collect()
        };

        if tied.len() == 1 {
            return Some(tied.into_iter().next().unwrap());
        }

        let mut guard = self.round_robin.lock().ok()?;
        let counter = guard.entry(role).or_insert(0);
        let idx = *counter;
        *counter = counter.wrapping_add(1);
        Some(tied[idx % tied.len()].clone())
    }
}

fn tokenize(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .collect()
}

fn score_persona(
    instr_tokens: &[String],
    def: &crate::agents::agent_loader::AgentDefinition,
) -> usize {
    let mut persona_tokens: Vec<String> = def
        .capabilities
        .iter()
        .flat_map(|c| tokenize(c))
        .collect();
    if let Some(persona) = &def.persona {
        for term in &persona.expertise {
            persona_tokens.extend(tokenize(term));
        }
    }
    instr_tokens
        .iter()
        .filter(|t| persona_tokens.iter().any(|p| p == *t))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::agent_loader::AgentDefinition;
    use crate::agents::AgentRegistry;
    use crate::types::{AgentPersona, AgentRole, PersonalityTraits, TaskProfile};

    fn make_persona(
        name: &str,
        role: AgentRole,
        expertise: &[&str],
        capabilities: &[&str],
    ) -> AgentDefinition {
        AgentDefinition {
            name: name.to_string(),
            description: String::new(),
            role,
            task_profile: TaskProfile::Coding,
            capabilities: capabilities.iter().map(|s| s.to_string()).collect(),
            system_prompt: format!("you are {name}"),
            instructions: String::new(),
            persona: Some(AgentPersona {
                display_name: name.to_string(),
                title: String::new(),
                github_username: name.to_lowercase(),
                avatar_url: None,
                bio: String::new(),
                personality: PersonalityTraits::default(),
                expertise: expertise.iter().map(|s| s.to_string()).collect(),
                communication_style: "neutral".to_string(),
            }),
        }
    }

    async fn registry_with(personas: Vec<AgentDefinition>) -> AgentRegistry {
        let dir = std::env::temp_dir().join(format!(
            "persona-router-test-{}",
            uuid::Uuid::new_v4()
        ));
        let team_dir = dir.join("team");
        std::fs::create_dir_all(&team_dir).unwrap();
        for def in &personas {
            crate::agents::agent_loader::save_agent_definition(&team_dir, def)
                .await
                .unwrap();
        }
        let reg = AgentRegistry::from_dir_with_fallback(&dir).await;
        let _ = std::fs::remove_dir_all(&dir);
        reg
    }

    #[tokio::test]
    async fn returns_none_when_no_persona_of_role() {
        let reg = registry_with(vec![]).await;
        let router = PersonaRouter::new(reg);
        assert!(router.pick_for_role(AgentRole::Coder, None, "task").is_none());
    }

    #[tokio::test]
    async fn keyword_match_beats_round_robin() {
        let reg = registry_with(vec![
            make_persona("Minho", AgentRole::Coder, &["rust", "backend"], &[]),
            make_persona("Yuna", AgentRole::Coder, &["react", "frontend"], &[]),
        ])
        .await;
        let router = PersonaRouter::new(reg);
        let pick = router.pick_for_role(AgentRole::Coder, None, "implement rust backend struct");
        assert_eq!(pick.as_deref(), Some("Minho"));
        let pick = router.pick_for_role(AgentRole::Coder, None, "build react frontend page");
        assert_eq!(pick.as_deref(), Some("Yuna"));
    }

    #[tokio::test]
    async fn round_robin_when_no_signal() {
        let reg = registry_with(vec![
            make_persona("Minho", AgentRole::Coder, &["rust"], &[]),
            make_persona("Yuna", AgentRole::Coder, &["react"], &[]),
        ])
        .await;
        let router = PersonaRouter::new(reg);
        // generic instructions with no expertise overlap
        let p1 = router
            .pick_for_role(AgentRole::Coder, None, "do the thing")
            .unwrap();
        let p2 = router
            .pick_for_role(AgentRole::Coder, None, "do the thing")
            .unwrap();
        assert_ne!(p1, p2);
    }

    #[tokio::test]
    async fn pool_filter_intersects_with_role() {
        let reg = registry_with(vec![
            make_persona("Minho", AgentRole::Coder, &["rust"], &[]),
            make_persona("Yuna", AgentRole::Coder, &["react"], &[]),
            make_persona("Jihun", AgentRole::Reviewer, &["arch"], &[]),
        ])
        .await;
        let router = PersonaRouter::new(reg);
        let pool = vec!["Yuna".to_string(), "Jihun".to_string()];
        // Coder role with pool ["Yuna", "Jihun"]: only Yuna is a Coder
        let pick = router.pick_for_role(AgentRole::Coder, Some(&pool), "anything");
        assert_eq!(pick.as_deref(), Some("Yuna"));
    }
}
