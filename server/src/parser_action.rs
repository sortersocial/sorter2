//! Parser output actions — graph handlers return these; HTML render turns them into markup.

#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    pub text: String,
    pub completion: String,
    pub description: Option<String>,
    pub score: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScrollingSuggestion {
    pub completion: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GuideOption {
    pub key: String,
    pub label: String,
    pub description: String,
    pub completion: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SuggestionsData {
    pub query: String,
    pub suggestion: Option<Suggestion>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ErrorData {
    pub error_type: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ParserAction {
    ShowSuggestions(SuggestionsData),
    ShowScrollingSuggestions {
        query: String,
        suggestions: Vec<ScrollingSuggestion>,
        interval_ms: u64,
        r#loop: bool,
    },
    ShowStaticGuide {
        query: String,
        title: String,
        subtitle: String,
        options: Vec<GuideOption>,
    },
    ShowMultiple {
        actions: Vec<ParserAction>,
    },
    ShowError(ErrorData),
    SuggestSubredditsFromDb {
        partial: String,
        prefix: String,
    },
    ResolveAndDisplaySubreddit {
        subreddit: String,
        prefix: String,
    },
    RenderEntityView {
        ns: String,
        pk: String,
    },
}

impl ParserAction {
    pub fn suggest(query: String, suggestion: Option<Suggestion>) -> Self {
        Self::ShowSuggestions(SuggestionsData { query, suggestion })
    }

    pub fn error(error_type: String, message: String) -> Self {
        Self::ShowError(ErrorData {
            error_type,
            message,
        })
    }

    pub fn multiple(actions: Vec<Self>) -> Self {
        Self::ShowMultiple { actions }
    }

    pub fn scrolling_suggestions(
        query: String,
        suggestions: Vec<ScrollingSuggestion>,
        interval_ms: u64,
        r#loop: bool,
    ) -> Self {
        Self::ShowScrollingSuggestions {
            query,
            suggestions,
            interval_ms,
            r#loop,
        }
    }

    pub fn guide(
        query: String,
        title: String,
        subtitle: String,
        options: Vec<GuideOption>,
    ) -> Self {
        Self::ShowStaticGuide {
            query,
            title,
            subtitle,
            options,
        }
    }

    /// Primary tab-completion string, if any.
    pub fn primary_completion(&self) -> Option<&str> {
        match self {
            Self::ShowSuggestions(data) => data
                .suggestion
                .as_ref()
                .map(|s| s.completion.as_str()),
            Self::ShowMultiple { actions } => actions.iter().find_map(|a| a.primary_completion()),
            _ => None,
        }
    }
}
