use maud::{html, Markup};

use crate::{
    form_template::template_json_compact,
    parser_action::{GuideOption, ParserAction, ScrollingSuggestion, Suggestion},
    ui_action::UI_RPC_FIELD,
};

const SAMPLE_SUBREDDITS: &[&str] = &["programming", "askreddit", "rust", "webdev", "aww"];

fn parse_query_rpc_template() -> String {
    template_json_compact(&serde_json::json!({
        "action": "parse_query",
        "query": {"$form": "query"},
    }))
    .expect("parse_query rpc template")
}

fn completion_button(completion: &str, label: &str, primary: bool) -> Markup {
    let class = if primary {
        "parser-completion parser-suggestion-primary btn-link"
    } else {
        "parser-completion btn-link"
    };
    html! {
        button
            type="button"
            class=(class)
            data-completion=(completion)
            title="Use this path" {
            (label)
        }
    }
}

fn render_suggestion(s: &Suggestion) -> Markup {
    let desc = s
        .description
        .as_deref()
        .unwrap_or("Tab to complete");
    html! {
        p class="parser-suggestion" {
            (completion_button(&s.completion, &s.completion, true))
            span class="muted small" { " — " (desc) }
        }
    }
}

fn render_scrolling(suggestions: &[ScrollingSuggestion]) -> Markup {
    html! {
        div class="parser-scrolling muted small" {
            p { "Examples:" }
            ul class="parser-scroll-list" {
                @for s in suggestions {
                    li { (completion_button(&s.completion, &s.completion, false)) }
                }
            }
        }
    }
}

fn render_guide(title: &str, subtitle: &str, options: &[GuideOption]) -> Markup {
    html! {
        div class="parser-guide" {
            h3 { (title) }
            p class="muted" { (subtitle) }
            ul class="parser-guide-list" {
                @for opt in options {
                    li {
                        strong { (opt.key) ": " }
                        (completion_button(&opt.completion, &opt.label, false))
                        span class="muted small" { " — " (opt.description) }
                    }
                }
            }
        }
    }
}

fn render_db_subs(partial: &str, prefix: &str) -> Markup {
    let needle = partial.to_lowercase();
    let matches: Vec<_> = SAMPLE_SUBREDDITS
        .iter()
        .filter(|s| s.contains(&needle) || prefix.ends_with('/') && needle.is_empty())
        .take(6)
        .collect();
    html! {
        div class="parser-db-subs muted small" {
            p { "Subreddits:" }
            ul {
                @for sub in matches {
                    @let completion = format!("{prefix}{sub}");
                    li { (completion_button(&completion, &format!("r/{sub}"), false)) }
                }
            }
        }
    }
}

fn render_action(action: &ParserAction) -> Markup {
    match action {
        ParserAction::ShowSuggestions(data) => html! {
            div class="parser-result parser-suggestions" {
                @if let Some(s) = &data.suggestion {
                    (render_suggestion(s))
                } @else {
                    p class="muted" { "No completion" }
                }
            }
        },
        ParserAction::ShowScrollingSuggestions { suggestions, .. } => {
            render_scrolling(suggestions)
        }
        ParserAction::ShowStaticGuide {
            title,
            subtitle,
            options,
            ..
        } => render_guide(title, subtitle, options),
        ParserAction::ShowMultiple { actions } => html! {
            div class="parser-multiple" {
                @for a in actions {
                    (render_action(a))
                }
            }
        },
        ParserAction::ShowError(data) => html! {
            p class="parser-error muted" {
                strong { (data.error_type) ": " }
                (data.message)
            }
        },
        ParserAction::SuggestSubredditsFromDb { partial, prefix } => {
            render_db_subs(partial, prefix)
        }
        ParserAction::ResolveAndDisplaySubreddit { subreddit, prefix } => html! {
            div class="parser-resolve" {
                p {
                    "Subreddit "
                    strong { "r/" (subreddit) }
                    @if subreddit.len() <= 3 {
                        span class="muted small" { " (partial — tab or pick a match)" }
                    }
                }
                p {
                    a class="parser-rank-link btn-link"
                        href=(format!("/?sub={subreddit}")) {
                        "Rank r/" (subreddit) " →"
                    }
                }
                (render_db_subs(subreddit, prefix))
            }
        },
        ParserAction::RenderEntityView { ns, pk } => html! {
            div class="parser-entity" {
                p {
                    "Would open "
                    code { (ns) "/" (pk) }
                }
            }
        },
    }
}

/// Parser output panel (inner content for `#parser-panel`).
pub fn parser_panel(query: &str, action: &ParserAction) -> Markup {
    html! {
        section id="parser-panel" class="demo-panel" {
            h2 { "Navigate" }
            p class="muted small" {
                "Type a Reddit path — "
                code { "r/rust" }
                ", "
                code { "reddit.com/r/programming/hot" }
                ", etc. Tab completes; each keystroke posts "
                code { "__rpc__" }
                " to "
                code { "/ui" }
                "."
            }
            form method="post" action="/ui" id="parser-form" {
                input
                    type="text"
                    name="query"
                    id="parser-input"
                    value=(query)
                    placeholder="r/ or reddit.com/…"
                    autocomplete="off"
                    spellcheck="false";
                input type="hidden" name=(UI_RPC_FIELD) value=(parse_query_rpc_template());
            }
            div id="parser-output" {
                @if query.is_empty() {
                    p class="muted" { "Start typing…" }
                } @else {
                    (render_action(action))
                }
            }
        }
    }
}

/// Wrap panel HTML for Idiomorph (morph `#parser-panel` only).
pub fn parser_panel_morph(query: &str, action: &ParserAction) -> Markup {
    parser_panel(query, action)
}
