use std::collections::HashMap;
use std::sync::OnceLock;

use crate::parser_action::{GuideOption, ParserAction, ScrollingSuggestion, Suggestion};

// --- Core Abstractions ---

/// Unique identifier for nodes in the graph
type NodeId = &'static str;

/// Pattern matching for edges
#[derive(Debug, Clone)]
pub enum EdgePattern {
    /// Matches exact literal string
    Literal(&'static str),
    
    /// Matches any prefix of a string and suggests the full string
    /// e.g., PrefixOf("reddit.com") matches "r", "re", "red", "reddit", "reddit.com"
    PrefixOf(&'static str),
    
    /// Captures a variable segment (e.g., subreddit name, username)
    Variable(&'static str),
    
    /// Matches any string (wildcard)
    Any,
}

impl EdgePattern {
    /// Try to match this pattern against input, return (consumed_chars, captured_value)
    fn matches(&self, input: &str) -> Option<(usize, Option<String>)> {
        match self {
            EdgePattern::Literal(lit) => {
                if input.starts_with(lit) {
                    Some((lit.len(), None))
                } else {
                    None
                }
            }
            EdgePattern::PrefixOf(target) => {
                // Check if input is a prefix of target
                if target.starts_with(input) && !input.is_empty() {
                    // It's a valid prefix
                    Some((input.len(), None))
                } else if input.starts_with(target) {
                    // Full match
                    Some((target.len(), None))
                } else {
                    None
                }
            }
            EdgePattern::Variable(var_name) => {
                // Consume until next '/' or end of string
                let end = input.find('/').unwrap_or(input.len());
                if end > 0 {
                    let captured = input[..end].to_string();
                    // Validate based on variable type
                    if is_valid_variable(var_name, &captured) {
                        Some((end, Some(captured)))
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            EdgePattern::Any => {
                // Match everything until next '/' or end
                let end = input.find('/').unwrap_or(input.len());
                if end > 0 {
                    Some((end, Some(input[..end].to_string())))
                } else {
                    None
                }
            }
        }
    }
    
    /// Get the completion suggestion for this pattern
    fn completion(&self, partial: &str) -> Option<String> {
        match self {
            EdgePattern::PrefixOf(target) => {
                if target.starts_with(partial) && partial != *target {
                    Some(target.to_string())
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

/// Edge in the graph
pub struct Edge {
    pattern: EdgePattern,
    target: NodeId,
    /// Optional description for autocomplete
    description: Option<&'static str>,
}

/// Handler function for generating UI actions (Send + Sync so the graph can live in `OnceLock`).
type Handler = Box<dyn Fn(&str, &str, &HashMap<String, String>) -> ParserAction + Send + Sync>;

/// Node in the graph
pub struct Node {
    #[allow(dead_code)]
    id: NodeId,
    edges: Vec<Edge>,
    handler: Option<Handler>,
}

/// The composable parser graph (immutable after `build`).
pub struct Graph {
    nodes: HashMap<NodeId, Node>,
    root: NodeId,
}

// --- Graph Builder (Fluent API) ---

pub struct GraphBuilder {
    nodes: HashMap<NodeId, Node>,
    current_node: Option<NodeId>,
    root: NodeId,
}

impl GraphBuilder {
    pub fn new() -> Self {
        let mut nodes = HashMap::new();
        nodes.insert(
            "root",
            Node {
                id: "root",
                edges: Vec::new(),
                handler: None,
            },
        );

        GraphBuilder {
            nodes,
            current_node: Some("root"),
            root: "root",
        }
    }

    /// Select a node to add edges to
    pub fn at(mut self, node_id: NodeId) -> Self {
        self.nodes.entry(node_id).or_insert_with(|| Node {
            id: node_id,
            edges: Vec::new(),
            handler: None,
        });
        self.current_node = Some(node_id);
        self
    }
    
    /// Add an edge from the current node
    pub fn edge(self, pattern: EdgePattern, target: NodeId) -> Self {
        self.edge_with_desc(pattern, target, None)
    }
    
    /// Add an edge with description
    pub fn edge_with_desc(
        mut self,
        pattern: EdgePattern,
        target: NodeId,
        desc: Option<&'static str>,
    ) -> Self {
        let current = self.current_node.expect("No current node selected");

        self.nodes.entry(target).or_insert_with(|| Node {
            id: target,
            edges: Vec::new(),
            handler: None,
        });

        if let Some(node) = self.nodes.get_mut(current) {
            node.edges.push(Edge {
                pattern,
                target,
                description: desc,
            });
        }

        self
    }

    /// Set handler for current node
    pub fn handler<F>(mut self, handler: F) -> Self
    where
        F: Fn(&str, &str, &HashMap<String, String>) -> ParserAction + Send + Sync + 'static,
    {
        let current = self.current_node.expect("No current node selected");
        if let Some(node) = self.nodes.get_mut(current) {
            node.handler = Some(Box::new(handler));
        }
        self
    }
    
    /// Build the final graph
    pub fn build(self) -> Graph {
        Graph {
            nodes: self.nodes,
            root: self.root,
        }
    }
}

// --- Parser Implementation ---

impl Graph {
    pub fn parse(&self, input: &str) -> ParserAction {
        let normalized = input.trim().to_lowercase();
        let mut state = ParserState {
            input: &normalized,
            cursor: 0,
            current_node_id: self.root,
            context: HashMap::new(),
            original_query: input.to_string(),
            current_prefix: String::new(),
        };
        
        self.parse_recursive(&mut state)
    }
    
    fn parse_recursive(&self, state: &mut ParserState) -> ParserAction {
        let node = self
            .nodes
            .get(state.current_node_id)
            .expect("Node not found in graph");

        // If we've consumed all input, check for handler or suggestions
        if state.cursor >= state.input.len() {
            if let Some(handler) = &node.handler {
                return handler(&state.original_query, &state.current_prefix, &state.context);
            }

            // No handler, try to suggest based on available edges
            return self.suggest_from_edges(node, state);
        }

        let remaining = &state.input[state.cursor..];

        // Try to match each edge
        for edge in &node.edges {
            if let Some((consumed, captured)) = edge.pattern.matches(remaining) {
                // Save state for potential backtracking
                let saved_cursor = state.cursor;
                let saved_node = state.current_node_id;
                let saved_prefix = state.current_prefix.clone();
                
                // Update state
                state.cursor += consumed;
                state.current_node_id = edge.target;
                state.current_prefix.push_str(&remaining[..consumed]);
                
                // Store captured variable if any
                if let Some(value) = captured {
                    if let EdgePattern::Variable(var_name) = &edge.pattern {
                        state.context.insert(var_name.to_string(), value);
                    }
                }
                
                // Check if this is a partial match that needs completion
                if state.cursor == state.input.len() {
                    if let Some(completion_suffix) = edge.pattern.completion(remaining) {
                        // Use the current_prefix plus the completion suffix
                        let full_completion = format!("{}{}", 
                            state.current_prefix, 
                            completion_suffix.strip_prefix(remaining).unwrap_or(&completion_suffix)
                        );
                        return ParserAction::suggest(
                            state.original_query.clone(),
                            Some(Suggestion {
                                text: full_completion.clone(),
                                completion: full_completion,
                                description: edge.description.map(|d| d.to_string()),
                                score: 1.0,
                            })
                        );
                    }
                }
                
                // Continue parsing from the target node
                let result = self.parse_recursive(state);
                
                // If we got a valid response, return it
                if !matches!(result, ParserAction::ShowError(_)) {
                    return result;
                }
                
                // Otherwise, restore state and try next edge
                state.cursor = saved_cursor;
                state.current_node_id = saved_node;
                state.current_prefix = saved_prefix;
            }
        }
        
        // No edges matched - try to provide suggestions
        self.suggest_from_edges(node, state)
    }
    
    fn suggest_from_edges(&self, node: &Node, state: &ParserState) -> ParserAction {
        let remaining = &state.input[state.cursor..];
        
        // Find edges that could match with more input
        for edge in &node.edges {
            match &edge.pattern {
                EdgePattern::PrefixOf(target) => {
                    if target.starts_with(remaining) && !remaining.is_empty() {
                        // Use current_prefix instead of rebuilding from input
                        let full_completion = format!("{}{}", state.current_prefix, target);
                        return ParserAction::suggest(
                            state.original_query.clone(),
                            Some(Suggestion {
                                text: full_completion.clone(),
                                completion: full_completion,
                                description: edge.description.map(|d| d.to_string()),
                                score: 1.0,
                            })
                        );
                    }
                }
                EdgePattern::Literal(lit) => {
                    if lit.starts_with(remaining) && !remaining.is_empty() {
                        let full_completion = format!("{}{}", state.current_prefix, lit);
                        return ParserAction::suggest(
                            state.original_query.clone(),
                            Some(Suggestion {
                                text: full_completion.clone(),
                                completion: full_completion,
                                description: edge.description.map(|d| d.to_string()),
                                score: 1.0,
                            })
                        );
                    }
                }
                _ => {}
            }
        }
        
        ParserAction::error(
            "InvalidPath".to_string(),
            format!("'{}' doesn't match any known pattern", state.original_query)
        )
    }
}

struct ParserState<'a> {
    input: &'a str,
    cursor: usize,
    current_node_id: NodeId,
    context: HashMap<String, String>,
    original_query: String,
    current_prefix: String,
}

// --- Helper Functions ---

fn is_valid_variable(var_name: &str, value: &str) -> bool {
    match var_name {
        "subreddit" => {
            !value.is_empty() && 
            value.len() <= 21 && 
            value.chars().all(|c| c.is_alphanumeric() || c == '_')
        }
        "username" => {
            !value.is_empty() && 
            value.len() <= 20 && 
            value.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-')
        }
        "post_id" => {
            !value.is_empty() && 
            value.len() <= 10 && 
            value.chars().all(|c| c.is_alphanumeric())
        }
        _ => true, // Allow any value for unknown variables
    }
}

// --- Define the Reddit Graph ---

pub fn build_reddit_graph() -> Graph {
    GraphBuilder::new()
        // === ROOT LEVEL: Direct aliases and domain/protocol patterns ===
        .at("root")
            // Direct aliases to subreddit and user selection
            .edge_with_desc(
                EdgePattern::PrefixOf("r/"),
                "subreddit_selection",
                Some("Browse subreddits (e.g., r/programming)")
            )
            .edge_with_desc(
                EdgePattern::PrefixOf("u/"),
                "user_selection",
                Some("Browse users (e.g., u/spez)")
            )
            
            // Reddit shortcuts - one pattern handles ALL prefixes!
            .edge_with_desc(
                EdgePattern::PrefixOf("reddit.com"), 
                "reddit_domain",
                Some("Go to Reddit")
            )
            
            // Protocol patterns - "h" can suggest "https://"
            .edge_with_desc(
                EdgePattern::PrefixOf("https://"),
                "https_protocol",
                Some("HTTPS protocol")
            )
            .edge_with_desc(
                EdgePattern::PrefixOf("http://"),
                "http_protocol", 
                Some("HTTP protocol")
            )
            .edge_with_desc(
                EdgePattern::PrefixOf("www."),
                "www_prefix",
                Some("World Wide Web")
            )
        

        
        // === HTTPS PROTOCOL: Can go to any domain ===
        .at("https_protocol")
            .edge_with_desc(
                EdgePattern::PrefixOf("reddit.com"),
                "reddit_domain",
                Some("Reddit (HTTPS)")
            )
            .edge_with_desc(
                EdgePattern::PrefixOf("www."),
                "https_www",
                Some("WWW prefix")
            )
        
        // === HTTP PROTOCOL: Similar to HTTPS ===
        .at("http_protocol")
            .edge_with_desc(
                EdgePattern::PrefixOf("reddit.com"),
                "reddit_domain",
                Some("Reddit (HTTP)")
            )
            .edge_with_desc(
                EdgePattern::PrefixOf("www."),
                "http_www",
                Some("WWW prefix")
            )
        
        // === HTTPS + WWW ===
        .at("https_www")
            .edge_with_desc(
                EdgePattern::PrefixOf("reddit.com"),
                "reddit_domain",
                Some("Reddit")
            )
        
        // === HTTP + WWW ===
        .at("http_www")
            .edge_with_desc(
                EdgePattern::PrefixOf("reddit.com"),
                "reddit_domain",
                Some("Reddit")
            )
        
        // === WWW PREFIX (without protocol) ===
        .at("www_prefix")
            .edge_with_desc(
                EdgePattern::PrefixOf("reddit.com"),
                "reddit_domain",
                Some("Reddit")
            )
        
        // === REDDIT DOMAIN: Expect "/" ===
        .at("reddit_domain")
            .edge(EdgePattern::Literal("/"), "reddit_root")
            .handler(|query, prefix, _ctx| {
                // If someone just types "reddit.com" (or with protocol) without slash
                // Suggest adding the slash using the current prefix
                let completion = format!("{}/", prefix);
                
                ParserAction::suggest(
                    query.to_string(),
                    Some(Suggestion {
                        text: completion.clone(),
                        completion,
                        description: Some("Continue to Reddit homepage".to_string()),
                        score: 1.0,
                    })
                )
            })
        
        // === REDDIT ROOT: The main Reddit navigation ===
        .at("reddit_root")
            .edge(EdgePattern::Literal("r/"), "subreddit_selection")
            .edge(EdgePattern::Literal("u/"), "user_selection")
            .handler(|query, prefix, _ctx| {
                ParserAction::multiple(vec![
                    ParserAction::scrolling_suggestions(
                        query.to_string(),
                        vec![
                            ScrollingSuggestion {
                                completion: format!("{}r/", prefix),
                            },
                            ScrollingSuggestion {
                                completion: format!("{}u/", prefix),
                            },
                        ],
                        1400, // 1.4 second interval (slower)
                        true  // loop through
                    ),
                    ParserAction::guide(
                        query.to_string(),
                        "Welcome to Sorter for Reddit".to_string(),
                        "Where would you like to start?".to_string(),
                        vec![
                            GuideOption { 
                                key: "r".to_string(), 
                                label: "Sort a Subreddit".to_string(), 
                                description: "Find the best posts in a community.".to_string(), 
                                completion: format!("{}r/", prefix)
                            },
                            GuideOption { 
                                key: "u".to_string(), 
                                label: "Sort User Content".to_string(), 
                                description: "Explore and rank a user's posts and comments.".to_string(), 
                                completion: format!("{}u/", prefix)
                            },
                        ]
                    ),
                ])
            })
        

        
        // === SUBREDDIT SELECTION: THE UNIFIED NODE ===
        // This node is now reached from `r/` OR `reddit.com/r/`
        .at("subreddit_selection")
            .edge(EdgePattern::Variable("subreddit"), "subreddit_page")
            .handler(|query, prefix, _ctx| {
                ParserAction::multiple(vec![
                    ParserAction::scrolling_suggestions(
                        query.to_string(),
                        vec![
                            ScrollingSuggestion {
                                completion: format!("{}programming", prefix),
                            },
                            ScrollingSuggestion {
                                completion: format!("{}askreddit", prefix),
                            },
                            ScrollingSuggestion {
                                completion: format!("{}aww", prefix),
                            },
                            ScrollingSuggestion {
                                completion: format!("{}rust", prefix),
                            },
                            ScrollingSuggestion {
                                completion: format!("{}webdev", prefix),
                            },
                        ],
                        1600, // 1.6 second interval (slower)
                        true  // loop through
                    ),
                    // Live DB-backed suggestions for subreddits as the user types
                    ParserAction::SuggestSubredditsFromDb { partial: query.to_string(), prefix: prefix.to_string() }
                ])
            })
        
        // === Specific subreddit page ===
        .at("subreddit_page")
            .edge(EdgePattern::Literal("/"), "subreddit_slash")
            .handler(|_query, prefix, ctx| {
                // Use the new unified subreddit resolution logic
                let subreddit = ctx.get("subreddit").cloned().unwrap_or_default();
                ParserAction::ResolveAndDisplaySubreddit {
                    subreddit,
                    prefix: prefix.to_string(),
                }
            })
        
        .at("subreddit_slash")
            .edge(EdgePattern::Literal("hot"), "subreddit_hot")
            .edge(EdgePattern::Literal("top"), "subreddit_top")
            .edge(EdgePattern::Literal("new"), "subreddit_new")
            .edge(EdgePattern::Literal("comments"), "subreddit_comments")
            .handler(|query, prefix, ctx| {
                let subreddit = ctx.get("subreddit").cloned().unwrap_or_default();
                ParserAction::guide(
                    query.to_string(), 
                    format!("What to sort in r/{}?", subreddit), 
                    "Choose a category to begin sorting.".to_string(), 
                    vec![
                        GuideOption { 
                            key: "hot".to_string(), 
                            label: "Hot Posts".to_string(), 
                            description: "Import and sort posts currently on the front page.".to_string(), 
                            completion: format!("{}hot", prefix) 
                        },
                        GuideOption { 
                            key: "top".to_string(), 
                            label: "Top Posts".to_string(), 
                            description: "Import and sort the highest-rated posts.".to_string(), 
                            completion: format!("{}top", prefix) 
                        },
                        GuideOption { 
                            key: "new".to_string(), 
                            label: "New Posts".to_string(), 
                            description: "Import and sort the newest posts.".to_string(), 
                            completion: format!("{}new", prefix) 
                        },
                        GuideOption { 
                            key: "comments".to_string(), 
                            label: "All Comments".to_string(), 
                            description: "Find the best comment across all imported threads.".to_string(), 
                            completion: format!("{}comments/", prefix) 
                        },
                    ]
                )
            })
        

        
        .at("user_selection")
            .edge(EdgePattern::Variable("username"), "user_profile")
            .handler(|query, prefix, _ctx| {
                ParserAction::guide(
                    query.to_string(),
                    "User Profile Sorting".to_string(),
                    "Enter a Reddit username to sort their content.".to_string(),
                    vec![
                        GuideOption {
                            key: "popular".to_string(),
                            label: "Popular Users".to_string(),
                            description: "Browse well-known Reddit users.".to_string(),
                            completion: prefix.to_string(),
                        },
                    ]
                )
            })
        
        // === Subreddit sort types ===
        .at("subreddit_hot")
            .handler(|_query, _prefix, ctx| {
                let subreddit = ctx.get("subreddit").cloned().unwrap_or_default();
                ParserAction::RenderEntityView {
                    ns: "reddit.subreddit".to_string(),
                    pk: subreddit,
                }
            })
        
        .at("subreddit_top")
            .handler(|_query, _prefix, ctx| {
                let subreddit = ctx.get("subreddit").cloned().unwrap_or_default();
                ParserAction::RenderEntityView {
                    ns: "reddit.subreddit".to_string(),
                    pk: subreddit,
                }
            })
        
        .at("subreddit_new")
            .handler(|_query, _prefix, ctx| {
                let subreddit = ctx.get("subreddit").cloned().unwrap_or_default();
                ParserAction::RenderEntityView {
                    ns: "reddit.subreddit".to_string(),
                    pk: subreddit,
                }
            })
        
        .at("subreddit_comments")
            .handler(|_query, _prefix, ctx| {
                let subreddit = ctx.get("subreddit").cloned().unwrap_or_default();
                ParserAction::RenderEntityView {
                    ns: "reddit.subreddit".to_string(),
                    pk: subreddit,
                }
            })
        
        // === User profile ===
        .at("user_profile")
            .handler(|_query, _prefix, ctx| {
                let username = ctx.get("username").cloned().unwrap_or_default();
                ParserAction::RenderEntityView {
                    ns: "reddit.user".to_string(),
                    pk: username,
                }
            })
        
        // === Build the graph ===
        .build()
}

// --- Public API ---

static REDDIT_GRAPH: OnceLock<Graph> = OnceLock::new();

fn reddit_graph() -> &'static Graph {
    REDDIT_GRAPH.get_or_init(build_reddit_graph)
}

/// Parse a query string and return a UI action
pub fn parse_reddit_url(query: &str) -> ParserAction {
    reddit_graph().parse(query)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Represents a keystroke action
    #[derive(Debug, Clone, PartialEq)]
    enum KeyAction {
        Type(String),  // Type characters
        Tab,           // Press tab (accept completion)

    }

    /// Expected state after a keystroke
    #[derive(Debug, Clone)]
    enum ExpectedAction {
        Suggestion { completion: String },
        ScrollingSuggestions { completions: Vec<String> },
        Guide { title_contains: String },

        RenderSubreddit { subreddit: String, sort: Option<String> },
        RenderSubredditComments { subreddit: String },
        RenderUser { username: String },
        ResolveSubreddit { subreddit: String, prefix: String },  // New unified subreddit resolution
        Error { error_type: String },
        Multiple { expected_actions: Vec<ExpectedAction> },  // Multiple responses with specific expectations
        MultipleAny,  // Multiple responses (any sub-actions - legacy)
        DbSuggestions { partial: String, prefix: String },  // Database-backed suggestions

    }

    impl ExpectedAction {
        fn matches(&self, action: &ParserAction) -> bool {
            match (self, action) {
                (ExpectedAction::Suggestion { completion }, ParserAction::ShowSuggestions(data)) => {
                    data.suggestion.as_ref()
                        .map(|s| s.completion == *completion)
                        .unwrap_or(false)
                }
                (ExpectedAction::ScrollingSuggestions { completions }, ParserAction::ShowScrollingSuggestions { suggestions, .. }) => {
                    let actual_completions: Vec<String> = suggestions.iter().map(|s| s.completion.clone()).collect();
                    *completions == actual_completions
                }
                (ExpectedAction::Guide { title_contains }, ParserAction::ShowStaticGuide { title, .. }) => {
                    title.contains(title_contains)
                }
                (ExpectedAction::RenderSubreddit { subreddit, sort: _ }, 
                 ParserAction::RenderEntityView { ns, pk }) => {
                    ns == "reddit.subreddit" && pk == subreddit
                }
                (ExpectedAction::RenderSubredditComments { subreddit }, 
                 ParserAction::RenderEntityView { ns, pk }) => {
                    ns == "reddit.subreddit" && pk == subreddit
                }
                (ExpectedAction::RenderUser { username }, ParserAction::RenderEntityView { ns, pk }) => {
                    ns == "reddit.user" && pk == username
                }
                (ExpectedAction::ResolveSubreddit { subreddit, prefix }, 
                 ParserAction::ResolveAndDisplaySubreddit { subreddit: s, prefix: p }) => {
                    s == subreddit && p == prefix
                }
                (ExpectedAction::Error { error_type }, ParserAction::ShowError(data)) => {
                    data.error_type == *error_type
                }
                (ExpectedAction::Multiple { expected_actions }, ParserAction::ShowMultiple { actions }) => {
                    // Check that all expected actions are present
                    if expected_actions.len() != actions.len() {
                        return false;
                    }
                    expected_actions.iter().zip(actions.iter()).all(|(expected, actual)| {
                        expected.matches(actual)
                    })
                }
                (ExpectedAction::MultipleAny, ParserAction::ShowMultiple { .. }) => true,
                (ExpectedAction::DbSuggestions { partial, prefix }, 
                 ParserAction::SuggestSubredditsFromDb { partial: p, prefix: pr }) => {
                    p == partial && pr == prefix
                }

                _ => false,
            }
        }
    }

    /// Test helper to simulate a sequence of keystrokes
    fn simulate_keystrokes(actions: Vec<KeyAction>) -> Vec<(String, ParserAction)> {
        let graph = build_reddit_graph();
        let mut current_text = String::new();
        let mut results = Vec::new();
        
        for action in actions {
            match action {
                KeyAction::Type(text) => {
                    current_text.push_str(&text);
                    let result = graph.parse(&current_text);
                    results.push((current_text.clone(), result));
                }
                KeyAction::Tab => {
                    // Tab accepts the current suggestion if there is one
                    let result = graph.parse(&current_text);
                    if let ParserAction::ShowSuggestions(ref data) = result {
                        if let Some(ref suggestion) = data.suggestion {
                            current_text = suggestion.completion.clone();
                            let new_result = graph.parse(&current_text);
                            results.push((current_text.clone(), new_result));
                        }
                    }
                }

            }
        }
        
        results
    }

    /// Test a flow using declarative (KeyAction, ExpectedAction) tuples
    fn test_flow(name: &str, flow: Vec<(KeyAction, ExpectedAction)>) {
        let graph = build_reddit_graph();
        let mut current_text = String::new();
        
        println!("\n=== Flow: {} ===", name);
        
        for (i, (key_action, expected)) in flow.iter().enumerate() {
            // Perform the keystroke
            match key_action {
                KeyAction::Type(text) => {
                    current_text.push_str(text);
                }
                KeyAction::Tab => {
                    // Tab accepts the current suggestion
                    let result = graph.parse(&current_text);
                    if let ParserAction::ShowSuggestions(data) = result {
                        if let Some(suggestion) = &data.suggestion {
                            current_text = suggestion.completion.clone();
                        }
                    }
                }
            }
            
            // Check the result
            let actual_action = graph.parse(&current_text);
            
            println!("  Step {}: {:?} -> '{}' -> {:?}", 
                     i + 1, key_action, current_text, actual_action);
            
            assert!(
                expected.matches(&actual_action),
                "Flow '{}' failed at step {}\n  Expected: {:?}\n  Actual: {:?}\n  Text: '{}'",
                name, i + 1, expected, actual_action, current_text
            );
        }
        
        println!("✓ Flow '{}' passed!", name);
    }
    
    /// Legacy helper for backward compatibility (will be removed)
    fn assert_flow(
        name: &str,
        actions: Vec<KeyAction>,
        expected_checks: Vec<Box<dyn Fn(&str, &ParserAction) -> bool>>,
    ) {
        let results = simulate_keystrokes(actions);
        
        println!("\n=== Flow: {} ===", name);
        for (i, (text, action)) in results.iter().enumerate() {
            println!("  Step {}: '{}' -> {:?}", i + 1, text, action);
            
            if i < expected_checks.len() {
                let check = &expected_checks[i];
                assert!(
                    check(text, action),
                    "Flow '{}' failed at step {} with text '{}' and action {:?}",
                    name, i + 1, text, action
                );
            }
        }
        println!("✓ Flow '{}' passed!", name);
    }

    #[test]
    fn test_https_reddit_tab_flow() {
        // Test: typing "https://reddit.com" and pressing tab should give "https://reddit.com/"
        assert_flow(
            "HTTPS Reddit with Tab",
            vec![
                KeyAction::Type("https://reddit.com".to_string()),
                KeyAction::Tab,
            ],
            vec![
                Box::new(|text, action| {
                    // After typing "https://reddit.com", should get a suggestion
                    text == "https://reddit.com" && matches!(action, ParserAction::ShowSuggestions(data) if 
                        data.suggestion.as_ref().map(|s| s.completion == "https://reddit.com/").unwrap_or(false)
                    )
                }),
                Box::new(|text, action| {
                    // After tab, should have "https://reddit.com/" and show guide
                    text == "https://reddit.com/" && matches!(action, ParserAction::ShowMultiple { .. })
                }),
            ],
        );
    }

    #[test]
    fn test_quick_subreddit_flow() {
        // Test: "r" -> TAB -> "rust" (direct alias flow)
        assert_flow(
            "Quick Subreddit Access",
            vec![
                KeyAction::Type("r".to_string()),
                KeyAction::Tab,
                KeyAction::Type("rust".to_string()),
            ],
            vec![
                Box::new(|text, action| {
                    // "r" should suggest "r/"
                    text == "r" && matches!(action, ParserAction::ShowSuggestions(data) if 
                        data.suggestion.as_ref().map(|s| s.completion == "r/").unwrap_or(false)
                    )
                }),
                Box::new(|text, action| {
                    // After tab, should have "r/" and show subreddit selection
                    text == "r/" && matches!(action, ParserAction::ShowMultiple { .. })
                }),
                Box::new(|text, action| {
                    // "r/rust" should resolve the subreddit
                    text == "r/rust" && matches!(action, ParserAction::ResolveAndDisplaySubreddit { subreddit, prefix } if subreddit == "rust" && prefix == "r/rust")
                }),
            ],
        );
    }

    #[test]
    fn test_progressive_completion_flow() {
        // Test progressive typing: "r" -> "re" -> "red" -> "redd" -> "reddit" -> TAB
        let progressive_actions = vec![
            KeyAction::Type("r".to_string()),
            KeyAction::Type("e".to_string()),
            KeyAction::Type("d".to_string()),
            KeyAction::Type("d".to_string()),
            KeyAction::Type("i".to_string()),
            KeyAction::Type("t".to_string()),
            KeyAction::Tab,
        ];
        
        let results = simulate_keystrokes(progressive_actions);
        
        println!("\n=== Progressive Completion Flow ===");
        for (i, (text, action)) in results.iter().enumerate() {
            println!("  '{}' -> {:?}", text, action);
            
            // First step: "r" should suggest "r/"
            if i == 0 && text == "r" {
                match action {
                    ParserAction::ShowSuggestions(data) => {
                        assert_eq!(
                            data.suggestion.as_ref().unwrap().completion,
                            "r/",
                            "Should suggest r/ at 'r'"
                        );
                    }
                    _ => panic!("Expected suggestion at 'r'"),
                }
            }
            // Other steps before tab should suggest "reddit.com"
            else if i > 0 && i < results.len() - 1 {
                match action {
                    ParserAction::ShowSuggestions(data) => {
                        assert_eq!(
                            data.suggestion.as_ref().unwrap().completion,
                            "reddit.com",
                            "Should suggest reddit.com at '{}'", text
                        );
                    }
                    _ => panic!("Expected suggestion at '{}'", text),
                }
            }
        }
        
        // After tab, should have "reddit.com"
        let (final_text, _) = results.last().unwrap();
        assert_eq!(final_text, "reddit.com");
        println!("✓ Progressive completion flow passed!");
    }

    #[test]
    fn test_subreddit_sort_flow() {
        // Test navigating to a subreddit and choosing a sort option
        assert_flow(
            "Subreddit Sort Navigation",
            vec![
                KeyAction::Type("reddit.com/r/programming/hot".to_string()),
            ],
            vec![
                Box::new(|text, action| {
                    text == "reddit.com/r/programming/hot" && 
                    matches!(action, ParserAction::RenderEntityView { ns, pk } 
                        if ns == "reddit.subreddit" && pk == "programming")
                }),
            ],
        );
    }

    #[test]
    fn test_user_profile_flow() {
        // Test navigating to a user profile
        assert_flow(
            "User Profile Navigation",
            vec![
                KeyAction::Type("reddit.com/u/spez".to_string()),
            ],
            vec![
                Box::new(|text, action| {
                    text == "reddit.com/u/spez" && 
                    matches!(action, ParserAction::RenderEntityView { ns, pk } if ns == "reddit.user" && pk == "spez")
                }),
            ],
        );
    }

    #[test]
    fn test_alias_shortcut_flow() {
        // Test using the "r/" shortcut - now correctly goes directly to subreddit
        assert_flow(
            "Alias Shortcut",
            vec![
                KeyAction::Type("r/".to_string()),
                KeyAction::Type("technology".to_string()),
            ],
            vec![
                Box::new(|text, action| {
                    text == "r/" && matches!(action, ParserAction::ShowMultiple { .. })
                }),
                Box::new(|text, action| {
                    text == "r/technology" && 
                    matches!(action, ParserAction::ResolveAndDisplaySubreddit { subreddit, prefix } if subreddit == "technology" && prefix == "r/technology")
                }),
            ],
        );
    }

    #[test]
    fn test_www_prefix_flow() {
        // Test with www prefix
        assert_flow(
            "WWW Prefix",
            vec![
                KeyAction::Type("www.reddit.com".to_string()),
                KeyAction::Tab,
            ],
            vec![
                Box::new(|text, action| {
                    // Should suggest adding slash
                    text == "www.reddit.com" && matches!(action, ParserAction::ShowSuggestions(data) if 
                        data.suggestion.as_ref().map(|s| s.completion == "www.reddit.com/").unwrap_or(false)
                    )
                }),
                Box::new(|text, action| {
                    // After tab, should show guide
                    text == "www.reddit.com/" && matches!(action, ParserAction::ShowMultiple { .. })
                }),
            ],
        );
    }

    #[test]
    fn test_invalid_path_handling() {
        // Test that invalid paths show errors
        assert_flow(
            "Invalid Path",
            vec![
                KeyAction::Type("reddit.com/invalid/path".to_string()),
            ],
            vec![
                Box::new(|text, action| {
                    text == "reddit.com/invalid/path" && 
                    matches!(action, ParserAction::ShowError(_))
                }),
            ],
        );
    }

    #[test]
    fn test_complete_user_journey() {
        // Test a complete user journey: type partial URL, tab complete, navigate to subreddit
        assert_flow(
            "Complete User Journey",
            vec![
                KeyAction::Type("http".to_string()),
                KeyAction::Type("s://r".to_string()),
                KeyAction::Tab,
                KeyAction::Type("/".to_string()),
                KeyAction::Type("r/".to_string()),
                KeyAction::Type("programming".to_string()),
                KeyAction::Type("/".to_string()),
                KeyAction::Type("top".to_string()),
            ],
            vec![
                Box::new(|text, action| {
                    // "http" should suggest "https://"
                    text == "http" && matches!(action, ParserAction::ShowSuggestions(data) if 
                        data.suggestion.as_ref().map(|s| s.completion == "https://").unwrap_or(false)
                    )
                }),
                Box::new(|text, action| {
                    // "https://r" should suggest "https://reddit.com"
                    text == "https://r" && matches!(action, ParserAction::ShowSuggestions(data) if 
                        data.suggestion.as_ref().map(|s| s.completion == "https://reddit.com").unwrap_or(false)
                    )
                }),
                Box::new(|text, action| {
                    // After tab, should have "https://reddit.com"
                    text == "https://reddit.com" && matches!(action, ParserAction::ShowSuggestions(_))
                }),
                Box::new(|text, action| {
                    // "https://reddit.com/" should show guide
                    text == "https://reddit.com/" && matches!(action, ParserAction::ShowMultiple { .. })
                }),
                Box::new(|text, action| {
                    // "https://reddit.com/r/" should show subreddit selection
                    text == "https://reddit.com/r/" && matches!(action, ParserAction::ShowMultiple { .. })
                }),
                Box::new(|text, action| {
                    // "https://reddit.com/r/programming" should resolve subreddit
                    text == "https://reddit.com/r/programming" && 
                    matches!(action, ParserAction::ResolveAndDisplaySubreddit { subreddit, prefix } 
                        if subreddit == "programming" && prefix == "https://reddit.com/r/programming")
                }),
                Box::new(|text, action| {
                    // "https://reddit.com/r/programming/" should show sort options
                    text == "https://reddit.com/r/programming/" && 
                    matches!(action, ParserAction::ShowStaticGuide { .. })
                }),
                Box::new(|text, action| {
                    // "https://reddit.com/r/programming/top" should render entity view
                    text == "https://reddit.com/r/programming/top" && 
                    matches!(action, ParserAction::RenderEntityView { ns, pk } 
                        if ns == "reddit.subreddit" && pk == "programming")
                }),
            ],
        );
    }

    #[test]
    fn test_multiple_tab_completions() {
        // Test multiple tab completions in sequence
        let actions = vec![
            KeyAction::Type("h".to_string()),
            KeyAction::Tab,  // Complete to "https://"
            KeyAction::Type("r".to_string()),
            KeyAction::Tab,  // Complete to "https://reddit.com"
            KeyAction::Type("/".to_string()),
        ];
        
        let results = simulate_keystrokes(actions);
        
        println!("\n=== Multiple Tab Completions ===");
        for (i, (text, _action)) in results.iter().enumerate() {
            println!("  Step {}: '{}'", i + 1, text);
        }
        
        // Verify the final state
        assert_eq!(results[1].0, "https://");  // After first tab
        assert_eq!(results[3].0, "https://reddit.com");  // After second tab
        assert_eq!(results[4].0, "https://reddit.com/");  // After typing /
        
        println!("✓ Multiple tab completions work correctly!");
    }

    // === CONVENIENCE MACROS FOR CLEANER TESTS ===
    
    macro_rules! flow {
        ($(($key:expr, $expected:expr)),* $(,)?) => {
            vec![$(($key, $expected)),*]
        };
    }
    
    macro_rules! type_text {
        ($text:expr) => {
            KeyAction::Type($text.to_string())
        };
    }
    
    macro_rules! suggests {
        ($completion:expr) => {
            ExpectedAction::Suggestion { completion: $completion.to_string() }
        };
    }
    
    macro_rules! renders_subreddit {
        ($subreddit:expr) => {
            ExpectedAction::RenderSubreddit { subreddit: $subreddit.to_string(), sort: None }
        };
        ($subreddit:expr, $sort:expr) => {
            ExpectedAction::RenderSubreddit { 
                subreddit: $subreddit.to_string(), 
                sort: Some($sort.to_string()) 
            }
        };
    }
    
    macro_rules! renders_subreddit_comments {
        ($subreddit:expr) => {
            ExpectedAction::RenderSubredditComments { subreddit: $subreddit.to_string() }
        };
    }
    
    macro_rules! resolves_subreddit {
        ($subreddit:expr, $prefix:expr) => {
            ExpectedAction::ResolveSubreddit { 
                subreddit: $subreddit.to_string(),
                prefix: $prefix.to_string()
            }
        };
    }
    
    macro_rules! shows_guide {
        ($title_contains:expr) => {
            ExpectedAction::Guide { title_contains: $title_contains.to_string() }
        };
    }
    
    macro_rules! multiple {
        ($($action:expr),* $(,)?) => {
            ExpectedAction::Multiple { expected_actions: vec![$($action),*] }
        };
    }
    
    macro_rules! scrolling_suggestions {
        ($($completion:expr),* $(,)?) => {
            ExpectedAction::ScrollingSuggestions { completions: vec![$($completion.to_string()),*] }
        };
    }
    
    macro_rules! db_suggestions {
        ($partial:expr, $prefix:expr) => {
            ExpectedAction::DbSuggestions { partial: $partial.to_string(), prefix: $prefix.to_string() }
        };
    }

    // === NEW DECLARATIVE TESTS ===

    #[test]
    fn test_declarative_https_tab_flow() {
        test_flow("HTTPS Tab Completion", vec![
            (KeyAction::Type("https://reddit.com".to_string()), 
             ExpectedAction::Suggestion { completion: "https://reddit.com/".to_string() }),
            (KeyAction::Tab, 
             multiple![
                 scrolling_suggestions!("https://reddit.com/r/", "https://reddit.com/u/"),
                 shows_guide!("Welcome to Sorter")
             ]),
        ]);
    }

    #[test]
    fn test_declarative_quick_subreddit_flow() {
        test_flow("Quick Subreddit Flow", vec![
            (KeyAction::Type("r".to_string()), 
             ExpectedAction::Suggestion { completion: "r/".to_string() }),
            (KeyAction::Tab, 
             ExpectedAction::MultipleAny), // r/ shows subreddit selection
            (KeyAction::Type("rust".to_string()),
             ExpectedAction::ResolveSubreddit { subreddit: "rust".to_string(), prefix: "r/rust".to_string() }),
        ]);
    }

    #[test]
    fn test_declarative_complete_journey() {
        test_flow("Complete User Journey", vec![
            (KeyAction::Type("http".to_string()),
             ExpectedAction::Suggestion { completion: "https://".to_string() }),
            (KeyAction::Type("s://r".to_string()),
             ExpectedAction::Suggestion { completion: "https://reddit.com".to_string() }),
            (KeyAction::Tab,
             ExpectedAction::Suggestion { completion: "https://reddit.com/".to_string() }),
            (KeyAction::Type("/r/programming/top".to_string()),
             ExpectedAction::RenderSubreddit { 
                 subreddit: "programming".to_string(), 
                 sort: Some("top".to_string()) 
             }),
        ]);
    }

    #[test]
    fn test_declarative_user_profile() {
        test_flow("User Profile Navigation", vec![
            (KeyAction::Type("reddit.com/u/spez".to_string()),
             ExpectedAction::RenderUser { username: "spez".to_string() }),
        ]);
    }

    #[test]
    fn test_declarative_error_handling() {
        test_flow("Error Handling", vec![
            (KeyAction::Type("reddit.com/invalid/path".to_string()),
             ExpectedAction::Error { error_type: "InvalidPath".to_string() }),
        ]);
    }

    #[test]
    fn test_declarative_progressive_completion() {
        test_flow("Progressive Completion", vec![
            (KeyAction::Type("r".to_string()),
             ExpectedAction::Suggestion { completion: "r/".to_string() }),
            (KeyAction::Type("e".to_string()),
             ExpectedAction::Suggestion { completion: "reddit.com".to_string() }),
            (KeyAction::Type("d".to_string()),
             ExpectedAction::Suggestion { completion: "reddit.com".to_string() }),
            (KeyAction::Type("dit".to_string()),
             ExpectedAction::Suggestion { completion: "reddit.com".to_string() }),
            (KeyAction::Tab,
             ExpectedAction::Suggestion { completion: "reddit.com/".to_string() }),
        ]);
    }

    #[test]
    fn test_declarative_subreddit_guide() {
        test_flow("Subreddit Guide", vec![
            (KeyAction::Type("reddit.com/r/programming/".to_string()),
             ExpectedAction::Guide { title_contains: "What to sort".to_string() }),
        ]);
    }

    #[test]
    fn test_clean_macro_example() {
        // This is what the tests can look like with macros!
        test_flow("Clean Macro Example", flow![
            (type_text!("r"), suggests!("r/")),
            (KeyAction::Tab, ExpectedAction::MultipleAny),
            (type_text!("rust/hot"), renders_subreddit!("rust", "hot")),
        ]);
    }

    #[test]
    fn test_unified_subreddit_resolution_flow() {
        // This test demonstrates the new unified behavior:
        // - r/programming (exact) → tries exact match first
        // - r/pro (partial) → tries exact match, then suggestions + TAB completion
        
        test_flow("Unified Resolution: Exact Match", flow![
            (type_text!("r/programming"), resolves_subreddit!("programming", "r/programming")),
        ]);
        
        test_flow("Unified Resolution: Partial Match", flow![
            (type_text!("r/pro"), resolves_subreddit!("pro", "r/pro")),
        ]);
        
        // Both go through the same action type, but dispatcher handles them differently:
        // - If "programming" exists in DB → shows EntityView immediately
        // - If "pro" doesn't exist in DB → shows Multiple with:
        //   1. Suggestions (for TAB completion to best match)  
        //   2. Selection (for clickable options including import)
    }

    #[test]
    fn test_tab_completion_workflow_unified() {
        // This demonstrates the desired TAB completion behavior:
        // r/pro + TAB → r/programming (if "programming" is the best DB match)
        
        // Note: This test shows the PARSER behavior. The actual TAB completion 
        // happens in the frontend when it receives the Multiple response containing
        // both Suggestions (for TAB) and Selection (for click options).
        
        let graph = build_reddit_graph();
        
        // 1. Parser generates unified action for partial input
        match graph.parse("r/pro") {
            ParserAction::ResolveAndDisplaySubreddit { subreddit, prefix } => {
                assert_eq!(subreddit, "pro");
                assert_eq!(prefix, "r/pro");
                println!("✓ Parser correctly identifies 'r/pro' as subreddit resolution");
            }
            _ => panic!("Expected ResolveAndDisplaySubreddit for 'r/pro'"),
        }
        
        // 2. When dispatcher runs (in real app), it will return Multiple response with:
        //    - Suggestions: { completion: "r/programming" } (for TAB)
        //    - Selection: [ "Import r/pro", "r/programming", ... ] (for clicks)
        
        println!("✓ TAB completion workflow: r/pro → ResolveAndDisplaySubreddit → Multiple(Suggestions + Selection)");
    }

    #[test]
    fn test_ultra_clean_user_journey() {
        test_flow("Ultra Clean User Journey", flow![
            (type_text!("h"), suggests!("https://")),
            (type_text!("ttps://r"), suggests!("https://reddit.com")), 
            (KeyAction::Tab, suggests!("https://reddit.com/")),
            (type_text!("/u/spez"), ExpectedAction::RenderUser { username: "spez".to_string() }),
        ]);
    }

    // === TESTS FROM parser.tdsl ===

    #[test]
    fn test_tdsl_basic_reddit_progression() {
        // Tests from parser.tdsl: r -> r/, then re -> reddit.com with all intermediate steps
        test_flow("TDSL Basic Reddit Progression", flow![
            (type_text!("r"), suggests!("r/")),
            (type_text!("e"), suggests!("reddit.com")),
            (type_text!("d"), suggests!("reddit.com")),
            (type_text!("d"), suggests!("reddit.com")),
            (type_text!("i"), suggests!("reddit.com")),
            (type_text!("t"), suggests!("reddit.com")),
            (type_text!("."), suggests!("reddit.com")),
            (type_text!("c"), suggests!("reddit.com")),
            (type_text!("o"), suggests!("reddit.com")),
            (KeyAction::Tab, suggests!("reddit.com/")),
        ]);
    }

    #[test]
    fn test_tdsl_reddit_com_slash_infographic() {
        // reddit.com/ -> {show infographic explaining that u (sort user posts) and r (sort subreddit posts)}
        test_flow("TDSL Reddit.com/ Infographic", flow![
            (type_text!("reddit.com/"), ExpectedAction::MultipleAny),
        ]);
    }

    #[test]
    fn test_tdsl_subreddit_selection() {
        // reddit.com/r/ -> reddit.com/r/{randomly chose sub from list}
        test_flow("TDSL Subreddit Selection", flow![
            (type_text!("reddit.com/r/"), ExpectedAction::MultipleAny),
        ]);
    }

    #[test]
    fn test_tdsl_subreddit_view() {
        // reddit.com/r/{sub} -> {resolve subreddit (exact match or suggestions)}
        test_flow("TDSL Subreddit View", flow![
            (type_text!("reddit.com/r/programming"), resolves_subreddit!("programming", "reddit.com/r/programming")),
        ]);
    }

    #[test]
    fn test_tdsl_subreddit_slash_infographic() {
        // reddit.com/r/{sub}/ -> {show infographic or something}
        test_flow("TDSL Subreddit Slash Infographic", flow![
            (type_text!("reddit.com/r/programming/"), shows_guide!("What to sort")),
        ]);
    }

    #[test]
    fn test_tdsl_subreddit_comments() {
        // reddit.com/r/{sub}/comments/{randomly chose comment from sql}
        test_flow("TDSL Subreddit Comments", flow![
            (type_text!("reddit.com/r/programming/comments"), renders_subreddit_comments!("programming")),
        ]);
    }

    #[test]
    fn test_tdsl_h_to_https() {
        // h->https:// (show supported domains)
        test_flow("TDSL H to HTTPS", flow![
            (type_text!("h"), suggests!("https://")),
        ]);
    }

    #[test]
    fn test_tdsl_composable_https_reddit() {
        // https://r->https://reddit.com/
        test_flow("TDSL Composable HTTPS Reddit", flow![
            (type_text!("https://r"), suggests!("https://reddit.com")),
        ]);
    }

    #[test]
    fn test_tdsl_composable_https_www() {
        // https://w->https://www.
        test_flow("TDSL Composable HTTPS WWW", flow![
            (type_text!("https://w"), suggests!("https://www.")),
        ]);
    }

    #[test]
    fn test_tdsl_composable_https_www_reddit() {
        // https://www.r->https://www.reddit.com/
        test_flow("TDSL Composable HTTPS WWW Reddit", flow![
            (type_text!("https://www.r"), suggests!("https://www.reddit.com")),
        ]);
    }

    #[test]
    fn test_tdsl_full_composable_chain() {
        // Complete chain showing composability: h -> https:// -> https://www.reddit.com
        test_flow("TDSL Full Composable Chain Step 1", flow![
            (type_text!("h"), suggests!("https://")),
        ]);
        
        test_flow("TDSL Full Composable Chain Step 2", flow![
            (type_text!("https://w"), suggests!("https://www.")),
        ]);
        
        test_flow("TDSL Full Composable Chain Step 3", flow![
            (type_text!("https://www.r"), suggests!("https://www.reddit.com")),
        ]);
    }

    #[test]
    fn test_tdsl_progressive_reddit_paths() {
        // Test various reddit paths work as expected
        test_flow("TDSL Progressive Reddit Paths", flow![
            (type_text!("reddit.com"), suggests!("reddit.com/")),
            (KeyAction::Tab, ExpectedAction::MultipleAny),
        ]);
    }

    #[test] 
    fn test_tdsl_subreddit_sorting_options() {
        // Test that subreddit sorting options work as described
        test_flow("TDSL Subreddit Sorting", flow![
            (type_text!("reddit.com/r/rust/hot"), renders_subreddit!("rust", "hot")),
        ]);
        
        test_flow("TDSL Subreddit Top", flow![
            (type_text!("reddit.com/r/rust/top"), renders_subreddit!("rust", "top")),
        ]);
        
        test_flow("TDSL Subreddit New", flow![
            (type_text!("reddit.com/r/rust/new"), renders_subreddit!("rust", "new")),
        ]);
    }

    #[test]
    fn test_tdsl_all_reddit_prefixes() {
        // Test all the prefixes mentioned in parser.tdsl work
        // "r" now suggests "r/", all others suggest "reddit.com"
        let prefixes_to_reddit = vec!["re", "red", "redd", "reddi", "reddit", "reddit.", "reddit.c", "reddit.co"];
        
        // Test "r" separately since it now suggests "r/"
        test_flow("TDSL Prefix: r", flow![
            (type_text!("r"), suggests!("r/")),
        ]);
        
        for prefix in prefixes_to_reddit {
            test_flow(&format!("TDSL Prefix: {}", prefix), flow![
                (type_text!(prefix), suggests!("reddit.com")),
            ]);
        }
    }

    #[test]
    fn test_tdsl_protocol_combinations() {
        // Test various protocol combinations from parser.tdsl
        let test_cases = vec![
            ("http://r", "http://reddit.com"),
            ("https://r", "https://reddit.com"),
            ("www.r", "www.reddit.com"),
            ("https://www.r", "https://www.reddit.com"),
            ("http://www.r", "http://www.reddit.com"),
        ];
        
        for (input, expected) in test_cases {
            test_flow(&format!("TDSL Protocol: {}", input), flow![
                (type_text!(input), suggests!(expected)),
            ]);
        }
    }

    #[test]
    fn test_tdsl_edge_case_completions() {
        // Test edge cases mentioned in parser.tdsl
        test_flow("TDSL Reddit.com completion", flow![
            (type_text!("reddit.com"), suggests!("reddit.com/")),
        ]);
        
        // Test that typing full reddit.com suggests the slash
        test_flow("TDSL Full domain completion", flow![
            (type_text!("reddit.com"), suggests!("reddit.com/")),
            (KeyAction::Tab, ExpectedAction::MultipleAny),
        ]);
    }

    #[test]
    fn test_real_user_trace_2025_08_07_fixed() {
        // Based on actual WebSocket trace from 2025-08-07T01:35:46Z
        // This test shows the CORRECT behavior after fixing the protocol preservation bug
        test_flow("Real User Trace: Progressive Typing with Tab Completions (Fixed)", flow![
            // User started typing "h"
            (type_text!("h"), suggests!("https://")),
            
            // User continued to "ht" 
            (type_text!("t"), suggests!("https://")),
            
            // User finished typing "https://" (trace shows full protocol)
            (type_text!("tps://"), suggests!("https://")),
            
            // User started typing "r" after protocol
            (type_text!("r"), suggests!("https://reddit.com")),
            
            // User continued typing "re"
            (type_text!("e"), suggests!("https://reddit.com")),
            
            // User typed out or completed "https://reddit.com"
            (type_text!("ddit.com"), suggests!("https://reddit.com/")),
            
            // User accepted completion to "https://reddit.com/"
            // NOW suggestions should preserve the https:// protocol
            (KeyAction::Tab, multiple![
                scrolling_suggestions!("https://reddit.com/r/", "https://reddit.com/u/"),  // This is the key fix!
                shows_guide!("Welcome to Sorter")
            ]),
        ]);
    }

    #[test]
    fn test_protocol_preservation_bug_fix() {
        // This test specifically verifies the fix for the protocol preservation bug
        test_flow("Protocol Preservation: HTTPS Reddit Homepage", flow![
            (type_text!("https://reddit.com/"), multiple![
                scrolling_suggestions!("https://reddit.com/r/", "https://reddit.com/u/"),  // Should preserve https://
                shows_guide!("Welcome to Sorter")
            ]),
        ]);
        
        // Test that subreddit selection preserves protocol
        test_flow("Protocol Preservation: HTTPS Subreddit Selection", flow![
            (type_text!("https://reddit.com/r/"), multiple![
                scrolling_suggestions!("https://reddit.com/r/programming", "https://reddit.com/r/askreddit", "https://reddit.com/r/aww", "https://reddit.com/r/rust", "https://reddit.com/r/webdev"),  // Should preserve https://
                db_suggestions!("https://reddit.com/r/", "https://reddit.com/r/")
            ]),
        ]);
        
        // Test with different protocols
        test_flow("Protocol Preservation: HTTP", flow![
            (type_text!("http://reddit.com/"), multiple![
                scrolling_suggestions!("http://reddit.com/r/", "http://reddit.com/u/"),  // Should preserve http://
                shows_guide!("Welcome to Sorter")
            ]),
        ]);
        
        test_flow("Protocol Preservation: WWW", flow![
            (type_text!("www.reddit.com/"), multiple![
                scrolling_suggestions!("www.reddit.com/r/", "www.reddit.com/u/"),  // Should preserve www.
                shows_guide!("Welcome to Sorter")
            ]),
        ]);
        
        // Test that plain reddit.com still works
        test_flow("Protocol Preservation: Plain Domain", flow![
            (type_text!("reddit.com/"), multiple![
                scrolling_suggestions!("reddit.com/r/", "reddit.com/u/"),  // No protocol prefix
                shows_guide!("Welcome to Sorter")
            ]),
        ]);
    }

    #[test]
    fn test_user_navigation_pattern() {
        // Models how users actually navigate: type → tab → click suggestion → end up at destination
        // This captures the "jump" from https://reddit.com/ to reddit.com/r/ seen in the trace
        test_flow("User Navigation: Protocol to Domain", flow![
            // User types and gets to homepage
            (type_text!("https://reddit.com/"), ExpectedAction::MultipleAny),
        ]);
        
        // Then they navigate (perhaps clicking a suggestion) to subreddit selection
        test_flow("User Navigation: Click to Subreddit", flow![
            // This is where they ended up - the suggestion in the guide probably said "reddit.com/r/"
            (type_text!("reddit.com/r/"), ExpectedAction::MultipleAny),
        ]);
    }

    #[test]
    fn test_progressive_typing_pattern() {
        // Based on the trace pattern - users often type character by character
        // This tests the exact sequence of suggestions they would see
        test_flow("Progressive Typing Pattern", flow![
            (type_text!("h"), suggests!("https://")),
            (type_text!("t"), suggests!("https://")), // Still suggests https://
            (type_text!("t"), suggests!("https://")), // ht -> htt, still suggests https://
            (type_text!("p"), suggests!("https://")), // http should still suggest https://
            (type_text!("s"), suggests!("https://")), // https should suggest https://
            (type_text!("://"), suggests!("https://")), // Even complete protocol still suggests itself
        ]);
    }

    #[test]  
    fn test_tab_completion_workflow() {
        // Test what happens when user uses tab completions strategically
        test_flow("Strategic Tab Completion Workflow", flow![
            // Start typing, get suggestion
            (type_text!("h"), suggests!("https://")),
            
            // Accept suggestion with tab - this should move us to "https://"
            (KeyAction::Tab, suggests!("https://")), // After tab, we're at "https://" which still suggests itself
            
            // Start typing reddit
            (type_text!("r"), suggests!("https://reddit.com")),
            
            // Accept reddit suggestion  
            (KeyAction::Tab, suggests!("https://reddit.com/")),
            
            // Accept final suggestion to get to homepage
            (KeyAction::Tab, ExpectedAction::MultipleAny),
        ]);
    }

    // Keep the original tests as well
    #[test]
    fn test_reddit_prefix_autocomplete() {
        let graph = build_reddit_graph();
        
        // "r" now suggests "r/", others suggest "reddit.com"
        match graph.parse("r") {
            ParserAction::ShowSuggestions(data) => {
                assert_eq!(data.suggestion.as_ref().unwrap().completion, "r/");
                println!("✓ 'r' → r/");
            }
            _ => panic!("Expected suggestion for 'r'"),
        }
        
        // Test other prefixes that should suggest "reddit.com"
        let prefixes = vec!["re", "red", "redd", "reddi", "reddit", "reddit.", "reddit.c", "reddit.co"];
        
        for prefix in prefixes {
            match graph.parse(prefix) {
                ParserAction::ShowSuggestions(data) => {
                    assert_eq!(data.suggestion.as_ref().unwrap().completion, "reddit.com");
                    println!("✓ '{}' → reddit.com", prefix);
                }
                _ => panic!("Expected suggestion for '{}'", prefix),
            }
        }
    }
    
    #[test]
    fn test_protocol_composition() {
        let graph = build_reddit_graph();
        
        // Test protocol + reddit compositions
        let tests = vec![
            ("https://r", "https://reddit.com"),
            ("https://re", "https://reddit.com"),
            ("https://reddit", "https://reddit.com"),
            ("https://www.r", "https://www.reddit.com"),
            ("https://www.reddit", "https://www.reddit.com"),
            ("http://r", "http://reddit.com"),
            ("www.r", "www.reddit.com"),
        ];
        
        for (input, expected) in tests {
            match graph.parse(input) {
                ParserAction::ShowSuggestions(data) => {
                    assert_eq!(data.suggestion.as_ref().unwrap().completion, expected);
                    println!("✓ '{}' → {}", input, expected);
                }
                _ => panic!("Expected suggestion for '{}'", input),
            }
        }
    }
    
    #[test]
    fn test_alias_and_full_paths() {
        let graph = build_reddit_graph();
        
        // reddit.com/ should show guide, r/ should show subreddit selection
        match graph.parse("reddit.com/") {
            ParserAction::ShowMultiple { actions } => {
                let has_guide = actions.iter()
                    .any(|a| matches!(a, ParserAction::ShowStaticGuide { .. }));
                assert!(has_guide, "Path 'reddit.com/' should show guide");
                println!("✓ 'reddit.com/' shows Reddit root guide");
            }
            _ => panic!("Expected Multiple action for 'reddit.com/'"),
        }
        
        match graph.parse("r/") {
            ParserAction::ShowMultiple { actions } => {
                let has_scrolling_suggestions = actions.iter()
                    .any(|a| matches!(a, ParserAction::ShowScrollingSuggestions { .. }));
                let has_db_suggestions = actions.iter()
                    .any(|a| matches!(a, ParserAction::SuggestSubredditsFromDb { .. }));
                assert!(has_scrolling_suggestions && has_db_suggestions, 
                       "Path 'r/' should show scrolling suggestions and DB suggestions");
                println!("✓ 'r/' shows subreddit selection");
            }
            _ => panic!("Expected Multiple action for 'r/'"),
        }
    }
    
    #[test]
    fn test_deep_navigation() {
        let graph = build_reddit_graph();
        
        // Test navigation to subreddit - now uses ResolveAndDisplaySubreddit
        match graph.parse("reddit.com/r/rust") {
            ParserAction::ResolveAndDisplaySubreddit { subreddit, prefix } => {
                assert_eq!(subreddit, "rust");
                assert_eq!(prefix, "reddit.com/r/rust");
                println!("✓ reddit.com/r/rust recognized");
            }
            _ => panic!("Expected ResolveAndDisplaySubreddit for subreddit"),
        }
        
        // Test with alias (r/ goes directly to subreddit, no double r/)
        match graph.parse("r/programming") {
            ParserAction::ResolveAndDisplaySubreddit { subreddit, prefix } => {
                assert_eq!(subreddit, "programming");
                assert_eq!(prefix, "r/programming");
                println!("✓ r/programming (alias) recognized");
            }
            _ => panic!("Expected ResolveAndDisplaySubreddit for subreddit via alias"),
        }
    }
}
