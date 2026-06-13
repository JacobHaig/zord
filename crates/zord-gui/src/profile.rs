//! Phase 48 — Person profile data types and pure helper functions.
//!
//! All computation here is read-only over existing store data.  No LLM is
//! used at any point.  The two pure functions are tested in the module-level
//! test block below.

// ── Data types ────────────────────────────────────────────────────────────────

/// One row in a person's meeting history.
#[derive(Debug, Clone, PartialEq)]
pub struct ProfileMeeting {
    pub session_id: String,
    /// Session title (may be empty — callers show a fallback "Recording").
    pub title: String,
    /// Session start time, epoch milliseconds.
    pub started_at: u64,
    /// Fraction of session speech time attributed to this speaker, `[0, 1]`.
    pub talk_share: f32,
    /// How many times this speaker started a segment while another was still
    /// speaking (from `SpeakerStats::interruptions_made`).
    pub interruptions: u32,
}

/// Full profile for one known speaker, assembled by the db thread.
#[derive(Debug, Clone, PartialEq)]
pub struct ProfileData {
    pub voiceprint_id: i64,
    pub name: String,
    /// Newest-first meeting list.
    pub meetings: Vec<ProfileMeeting>,
    /// Open `- [ ]` task-list items from the Overview whose text contains the
    /// person's name (case-insensitive).  Capped at 20.
    pub open_items: Vec<String>,
    /// TF-IDF top terms distinctive to this person's transcript lines, up to
    /// `k=6` terms.
    pub topics: Vec<String>,
    /// `max(started_at)` over all appearances, epoch milliseconds.
    pub last_heard_ms: u64,
}

// ── Pure helpers ──────────────────────────────────────────────────────────────

/// Extract open task-list items from a markdown overview document that contain
/// `name` (case-insensitive).
///
/// # Rules
/// - A line is considered a task-list item when it matches the pattern
///   `^ *- \[ \] ` (an unchecked box; leading spaces allowed).
/// - Checked boxes (`- [x]` / `- [X]`) are **excluded**.
/// - The marker prefix is stripped and the task text is trimmed.
/// - The name match is case-insensitive and word-boundary anchored, so a
///   short name like "Al" does not match "Alice"/"Malcolm".
/// - At most 20 items are returned (document order, first match wins).
/// - No special handling for fenced code blocks is performed — the caller
///   controls what document text is passed in, and keeping the scan simple
///   is an intentional design choice noted here.
pub fn overview_items_for(doc: &str, name: &str) -> Vec<String> {
    if name.trim().is_empty() {
        return Vec::new();
    }
    let name_lower = name.to_lowercase();
    let mut out = Vec::new();
    for line in doc.lines() {
        let trimmed = line.trim_start();
        // Match unchecked task-list markers only. The bullet may be followed
        // by extra spaces (`- [ ]  text`), so trim the captured task text.
        if let Some(rest) = trimmed.strip_prefix("- [ ]") {
            let rest = rest.trim();
            if contains_word(&rest.to_lowercase(), &name_lower) {
                out.push(rest.to_string());
                if out.len() >= 20 {
                    break;
                }
            }
        }
    }
    out
}

/// Case-insensitive, word-boundary-anchored containment: `needle` must be
/// flanked by non-alphanumeric chars (or the string ends). Both args should
/// already be lowercased. Prevents "al" from matching "Alice". Char-aware so
/// non-ASCII names (e.g. unicode display names) behave correctly.
fn contains_word(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let mut from = 0;
    while let Some(rel) = haystack[from..].find(needle) {
        let start = from + rel;
        let end = start + needle.len();
        let before_alnum = haystack[..start]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric());
        let after_alnum = haystack[end..]
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric());
        if !before_alnum && !after_alnum {
            return true;
        }
        // Advance one char past this occurrence and keep searching.
        from = start + haystack[start..].chars().next().map_or(1, char::len_utf8);
    }
    false
}

/// Built-in English stopword list (~50 common words) used by TF-IDF.
const STOPWORDS: &[&str] = &[
    "the", "and", "for", "are", "but", "not", "you", "all", "any", "can", "had", "her", "was",
    "one", "our", "out", "day", "get", "has", "him", "his", "how", "its", "let", "may", "now",
    "old", "see", "two", "way", "who", "did", "yes", "yet", "use", "via", "per", "due", "set",
    "got", "isn", "don", "won", "been", "from", "that", "this", "they", "them", "then", "than",
    "with", "will", "what", "when", "have", "just", "like", "into", "some", "more", "also", "very",
    "each",
];

/// Tokenize text into lowercase alphanumeric words of length ≥ 3, excluding
/// stopwords.
fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 3)
        .map(|w| w.to_lowercase())
        .filter(|w| !STOPWORDS.contains(&w.as_str()))
        .collect()
}

/// Compute the top-`k` TF-IDF terms that are most distinctive to
/// `person_lines` compared to `other_lines`.
///
/// # Algorithm (simplest honest variant)
/// - Treat each **line** as a "document"; total corpus = person + other lines.
/// - TF(term, person) = count(term in person_lines) / max(1, total person tokens)
/// - IDF(term) = ln(1 + total_docs / df) where df = number of lines containing
///   the term (at least once) across the **whole** corpus.
/// - Score = TF × IDF.  Top `k` by score, ties broken alphabetically.
///
/// Returns an empty vec when `person_lines` is empty.  Terms present only in
/// the other-lines corpus (df > 0 but TF = 0) score zero and are excluded.
pub fn tfidf_topics(person_lines: &[String], other_lines: &[String], k: usize) -> Vec<String> {
    if person_lines.is_empty() || k == 0 {
        return Vec::new();
    }

    // Build person token counts.
    let mut person_counts: std::collections::HashMap<String, u32> =
        std::collections::HashMap::new();
    let mut person_total: u32 = 0;
    for line in person_lines {
        for tok in tokenize(line) {
            *person_counts.entry(tok).or_insert(0) += 1;
            person_total += 1;
        }
    }
    if person_total == 0 {
        return Vec::new();
    }

    // Document frequency: number of lines (across both corpora) that contain
    // the term at least once.
    let total_docs = (person_lines.len() + other_lines.len()) as f64;
    let mut df: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    for line in person_lines.iter().chain(other_lines.iter()) {
        // Use a set per line to count each term at most once.
        let terms: std::collections::HashSet<String> = tokenize(line).into_iter().collect();
        for t in terms {
            *df.entry(t).or_insert(0) += 1;
        }
    }

    // Score every term that appears in person_lines.
    let mut scored: Vec<(String, f64)> = person_counts
        .iter()
        .map(|(term, &count)| {
            let tf = count as f64 / person_total as f64;
            let d = *df.get(term).unwrap_or(&1) as f64;
            let idf = (1.0 + total_docs / d).ln();
            (term.clone(), tf * idf)
        })
        .collect();

    // Sort: score descending, then alphabetical for determinism.
    scored.sort_by(|(ta, sa), (tb, sb)| {
        sb.partial_cmp(sa)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| ta.cmp(tb))
    });

    scored.into_iter().take(k).map(|(t, _)| t).collect()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── overview_items_for ────────────────────────────────────────────────────

    /// Unchecked items containing the name are returned.
    #[test]
    fn overview_items_basic() {
        let doc = "- [ ] Alice needs to review the PR\n- [ ] Bob will fix the bug\n";
        let items = overview_items_for(doc, "Alice");
        assert_eq!(items, vec!["Alice needs to review the PR"]);
    }

    /// Checked items (`- [x]`) are excluded even when name matches.
    #[test]
    fn overview_items_checked_excluded() {
        let doc = "- [x] Alice completed task\n- [ ] Alice has another task\n";
        let items = overview_items_for(doc, "Alice");
        assert_eq!(items, vec!["Alice has another task"]);
    }

    /// Name matching is case-insensitive.
    #[test]
    fn overview_items_case_insensitive() {
        let doc = "- [ ] ALICE should do this\n- [ ] alice also this\n";
        let items = overview_items_for(doc, "Alice");
        assert_eq!(items.len(), 2);
        assert!(items[0].contains("ALICE"));
        assert!(items[1].contains("alice"));
    }

    /// `- [X]` (uppercase X) is also excluded.
    #[test]
    fn overview_items_checked_uppercase_x_excluded() {
        let doc = "- [X] Alice done\n- [ ] Alice todo\n";
        let items = overview_items_for(doc, "Alice");
        assert_eq!(items, vec!["Alice todo"]);
    }

    /// Items not containing the name are not returned.
    #[test]
    fn overview_items_no_match() {
        let doc = "- [ ] Bob does something\n- [ ] Carol too\n";
        let items = overview_items_for(doc, "Alice");
        assert!(items.is_empty());
    }

    /// Empty document returns empty.
    #[test]
    fn overview_items_empty_doc() {
        assert!(overview_items_for("", "Alice").is_empty());
    }

    /// Word-boundary anchored: a short name must not match a longer word,
    /// but a real occurrence (flanked by punctuation/space) must. Also trims
    /// extra spaces after the marker.
    #[test]
    fn overview_items_word_boundary_and_trim() {
        let doc = "- [ ] Malcolm reviews the PR\n- [ ]  Ask Al, then ship\n";
        // "Al" is a substring of "Malcolm" but only a real word in line 2.
        let items = overview_items_for(doc, "Al");
        assert_eq!(items, vec!["Ask Al, then ship".to_string()]);
        // Full-name match still works and the double-space is trimmed.
        assert_eq!(
            overview_items_for("- [ ]  Malcolm owns infra\n", "malcolm"),
            vec!["Malcolm owns infra".to_string()]
        );
    }

    /// At most 20 items returned.
    #[test]
    fn overview_items_cap_20() {
        let doc: String = (0..25).map(|i| format!("- [ ] Alice task {i}\n")).collect();
        let items = overview_items_for(&doc, "Alice");
        assert_eq!(items.len(), 20);
    }

    // ── tfidf_topics ─────────────────────────────────────────────────────────

    /// A distinctive term in person_lines ranks above a common term.
    #[test]
    fn tfidf_distinctive_term_ranks_higher() {
        // "kubernetes" appears only in person_lines; "the" would be filtered by
        // stopwords but we use a unique technical term vs a common filler word.
        let person: Vec<String> = vec![
            "kubernetes deployment failed".to_string(),
            "kubernetes pods crashing".to_string(),
            "kubernetes cluster needs upgrade".to_string(),
        ];
        // Other side uses generic words, not kubernetes.
        let other: Vec<String> = vec![
            "meeting starts now".to_string(),
            "okay let discuss".to_string(),
            "thanks everyone for joining".to_string(),
        ];
        let topics = tfidf_topics(&person, &other, 6);
        // "kubernetes" should appear as the top topic.
        assert!(
            topics.contains(&"kubernetes".to_string()),
            "expected 'kubernetes' in {topics:?}"
        );
    }

    /// Stopwords are not returned as topics.
    #[test]
    fn tfidf_stopwords_excluded() {
        let person: Vec<String> = vec![
            "the and for are but you all any can had".to_string(),
            "kubernetes is great".to_string(),
        ];
        let other: Vec<String> = vec!["other content here".to_string()];
        let topics = tfidf_topics(&person, &other, 10);
        let stopwords_found: Vec<_> = topics
            .iter()
            .filter(|t| STOPWORDS.contains(&t.as_str()))
            .collect();
        assert!(
            stopwords_found.is_empty(),
            "stopwords found in topics: {stopwords_found:?}"
        );
    }

    /// Empty person_lines returns empty.
    #[test]
    fn tfidf_empty_person() {
        let topics = tfidf_topics(&[], &["some content".to_string()], 6);
        assert!(topics.is_empty());
    }

    /// Empty other_lines still works (just uses person corpus for IDF).
    #[test]
    fn tfidf_empty_other() {
        let person = vec!["kubernetes deployment cluster".to_string()];
        let topics = tfidf_topics(&person, &[], 3);
        assert!(!topics.is_empty());
    }

    /// k=0 returns empty.
    #[test]
    fn tfidf_k_zero() {
        let person = vec!["kubernetes deployment".to_string()];
        let other = vec!["other stuff".to_string()];
        assert!(tfidf_topics(&person, &other, 0).is_empty());
    }

    /// Returns at most k terms.
    #[test]
    fn tfidf_at_most_k() {
        let person: Vec<String> = (0..20).map(|i| format!("uniqueterm{i} text")).collect();
        let other: Vec<String> = vec![];
        let topics = tfidf_topics(&person, &other, 6);
        assert!(topics.len() <= 6);
    }
}
