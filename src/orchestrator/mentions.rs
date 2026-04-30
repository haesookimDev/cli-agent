//! `@persona` mention parsing.
//!
//! Used by `submit_run` to (1) extract `@Name` tokens from a user task so
//! the first match can drive the run as the assignee and the rest can be
//! pre-loaded into recipients' inboxes, and (2) by node_executor to scan
//! agent outputs for cross-persona handoffs.
//!
//! Match rules: token must be preceded by start-of-string or whitespace
//! (so it isn't an email address suffix), then `@` plus at least 2
//! letters/digits/underscores/hyphens. Code blocks (``` … ```) are
//! stripped before scanning so `@Bob` in code stays inert.

use std::collections::HashSet;

/// Strip fenced code blocks (```...```) and inline backticks (`...`) so
/// we don't pick up mentions that are clearly code, not addressing.
fn strip_code(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '`' {
            // Possible fenced code block (```).
            if matches!(chars.peek(), Some('`'))
                && chars.clone().nth(1) == Some('`')
            {
                chars.next();
                chars.next();
                while let Some(c) = chars.next() {
                    if c == '`' && chars.peek() == Some(&'`') {
                        chars.next();
                        if chars.peek() == Some(&'`') {
                            chars.next();
                            break;
                        }
                    }
                }
                continue;
            }
            // Inline backticks: skip until next `.
            while let Some(c) = chars.next() {
                if c == '`' {
                    break;
                }
            }
            continue;
        }
        out.push(ch);
    }
    out
}

/// Extract `@Name` mentions from `text`. Filters down to names that
/// actually exist in `registered` (case-insensitive). When two registered
/// personas differ only in case, returns only the canonical name as
/// stored in `registered`.
pub fn parse_mentions(text: &str, registered: &[String]) -> Vec<String> {
    let cleaned = strip_code(text);
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let bytes = cleaned.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'@' {
            i += 1;
            continue;
        }
        // Must be at start or after whitespace / non-word char.
        if i > 0 {
            let prev = bytes[i - 1];
            if prev.is_ascii_alphanumeric() || prev == b'_' {
                i += 1;
                continue;
            }
        }
        let start = i + 1;
        let mut end = start;
        while end < bytes.len() {
            let c = bytes[end];
            if c.is_ascii_alphanumeric() || c == b'_' || c == b'-' || c == b' ' {
                end += 1;
            } else {
                break;
            }
        }
        // Trim trailing whitespace from the candidate slice.
        let mut candidate_end = end;
        while candidate_end > start
            && bytes[candidate_end - 1] == b' '
        {
            candidate_end -= 1;
        }
        let candidate = &cleaned[start..candidate_end];
        if candidate.len() < 2 {
            i = end;
            continue;
        }
        // Try the longest registered prefix first so "@Senior Dev Minho"
        // beats "@Senior".
        let lower = candidate.to_ascii_lowercase();
        let mut best: Option<&str> = None;
        for reg in registered {
            let rl = reg.to_ascii_lowercase();
            if lower.starts_with(&rl)
                && (best.map(|b| b.len()).unwrap_or(0) < reg.len())
            {
                best = Some(reg.as_str());
            }
        }
        if let Some(name) = best {
            if seen.insert(name.to_string()) {
                out.push(name.to_string());
            }
            i = start + name.len();
            continue;
        }
        i = end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn personas() -> Vec<String> {
        vec![
            "Senior Dev Minho".to_string(),
            "Tech Lead Jihun".to_string(),
            "QA Engineer Taewon".to_string(),
        ]
    }

    #[test]
    fn picks_up_simple_mention() {
        let m = parse_mentions("Hi @Senior Dev Minho can you check?", &personas());
        assert_eq!(m, vec!["Senior Dev Minho"]);
    }

    #[test]
    fn case_insensitive_match() {
        let m = parse_mentions("Yo @senior dev minho", &personas());
        assert_eq!(m, vec!["Senior Dev Minho"]);
    }

    #[test]
    fn skips_unknown_handles() {
        let m = parse_mentions("Hey @nobody and @Senior Dev Minho", &personas());
        assert_eq!(m, vec!["Senior Dev Minho"]);
    }

    #[test]
    fn ignores_mentions_inside_code_fences() {
        let text = "```\n@Senior Dev Minho should not match\n```\nbut @Tech Lead Jihun should";
        let m = parse_mentions(text, &personas());
        assert_eq!(m, vec!["Tech Lead Jihun"]);
    }

    #[test]
    fn ignores_email_like_at() {
        let m = parse_mentions("ping me at me@example.com or @Tech Lead Jihun", &personas());
        assert_eq!(m, vec!["Tech Lead Jihun"]);
    }

    #[test]
    fn dedupes_repeats() {
        let m = parse_mentions(
            "@Senior Dev Minho also @Senior Dev Minho",
            &personas(),
        );
        assert_eq!(m, vec!["Senior Dev Minho"]);
    }

    #[test]
    fn order_preserved() {
        let m = parse_mentions("@QA Engineer Taewon then @Tech Lead Jihun", &personas());
        assert_eq!(m, vec!["QA Engineer Taewon", "Tech Lead Jihun"]);
    }
}
