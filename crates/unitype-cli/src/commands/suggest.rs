//! `unitype suggest` — what looks like the same thing described twice?

use serde_json::json;

use super::Output;
use crate::Failure;
use crate::project::Project;

/// Reports correspondences between universes that nobody has declared.
///
/// Only a suggestion: whether two universes are talking about the same thing is
/// a judgement about the world, and a person makes it.
pub fn run(project: &Project, min: f32, out: &Output) -> Result<(), Failure> {
    let found = project.workspace.suggest_links(min);

    out.result(
        json!(
            found
                .iter()
                .map(|s| json!({
                    "from": s.from.to_string(),
                    "to": s.to.to_string(),
                    "score": s.score,
                    "lossy": s.lossy,
                }))
                .collect::<Vec<_>>()
        ),
        || {
            if found.is_empty() {
                out.say("Nothing looks like the same thing described twice.");
                return;
            }
            out.say("These might be the same thing. Declaring a link is up to you.");
            out.say("");
            for suggestion in &found {
                out.say(format!(
                    "  {} -> {}   {:.0}% match{}",
                    suggestion.from,
                    suggestion.to,
                    suggestion.score * 100.0,
                    if suggestion.lossy {
                        ", but fields would be dropped"
                    } else {
                        ""
                    }
                ));
            }
        },
    );
    Ok(())
}
