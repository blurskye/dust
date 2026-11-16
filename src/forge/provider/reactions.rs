//! Reactions on a pull request's or issue's body where the forge keeps them apart from the
//! item: Gitea's `issues/{n}/reactions` and GitLab's `award_emoji`, one request per item,
//! counted into GitHub's words and order (`+1`, `-1`, `laugh`, `hooray`, `confused`, `heart`,
//! `rocket`, `eyes`). Other emoji have no place in that set and are left out.
use crate::forge::model::Reaction;
use serde_json::Value;

const ORDER: [&str; 8] = [
    "+1", "-1", "laugh", "hooray", "confused", "heart", "rocket", "eyes",
];

/// Gitea's (and Forgejo's) reaction list: one `{ "content": "+1", "user": … }` per person.
pub(super) fn gitea(list: &Value) -> Vec<Reaction> {
    tally(list, |each| each["content"].as_str().map(str::to_owned))
}

/// GitLab's award emoji: one `{ "name": "thumbsup", "user": … }` per person.
pub(super) fn gitlab(list: &Value) -> Vec<Reaction> {
    tally(list, |each| {
        let word = match each["name"].as_str()? {
            "thumbsup" => "+1",
            "thumbsdown" => "-1",
            "laughing" | "smile" => "laugh",
            "tada" => "hooray",
            "confused" => "confused",
            "heart" => "heart",
            "rocket" => "rocket",
            "eyes" => "eyes",
            _ => return None,
        };
        Some(word.to_owned())
    })
}

fn tally(list: &Value, word: impl Fn(&Value) -> Option<String>) -> Vec<Reaction> {
    let words = list
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(word)
        .collect::<Vec<_>>();
    ORDER
        .iter()
        .map(|content| Reaction {
            content: (*content).to_owned(),
            count: words.iter().filter(|word| word == content).count() as u64,
        })
        .filter(|reaction| reaction.count > 0)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pairs(reactions: Vec<Reaction>) -> Vec<(String, u64)> {
        reactions
            .into_iter()
            .map(|reaction| (reaction.content, reaction.count))
            .collect()
    }

    #[test]
    fn gitea_reactions_are_counted_in_github_order() {
        let list = json!([
            { "content": "heart", "user": { "login": "a" } },
            { "content": "+1", "user": { "login": "a" } },
            { "content": "+1", "user": { "login": "b" } },
            { "content": "custom", "user": { "login": "c" } },
        ]);
        assert_eq!(pairs(gitea(&list)), [("+1".into(), 2), ("heart".into(), 1)]);
        assert!(gitea(&json!({ "message": "not found" })).is_empty());
    }

    #[test]
    fn gitlab_award_emoji_take_github_words() {
        let list = json!([
            { "name": "thumbsup" },
            { "name": "tada" },
            { "name": "thumbsup" },
            { "name": "thumbsdown" },
            { "name": "8ball" },
        ]);
        assert_eq!(
            pairs(gitlab(&list)),
            [("+1".into(), 2), ("-1".into(), 1), ("hooray".into(), 1)]
        );
    }
}
