//! Identity for a resolved engine operation, separate from model arguments.
use serde_json::{Value, json};

pub const META: &str = "com.microsoft.amplifier.tool";

pub enum Identity<'a> {
    Builtin(&'a str),
    Mcp { server: &'a str, name: &'a str },
}

impl Identity<'_> {
    pub fn call(&self, id: &str, title: &str, kind: &str, input: &Value) -> Value {
        let identity = match self {
            Self::Builtin(name) => json!({"version":1,"kind":"builtin","name":name}),
            Self::Mcp { server, name } => {
                json!({"version":1,"kind":"mcp","server":server,"name":name})
            }
        };
        let mut call = json!({"toolCallId":id,"title":title,"kind":kind,
            "status":"pending","rawInput":input});
        call["_meta"][META] = identity;
        call
    }
}

/// What the model is told when it asks for a tool this session does not list.
///
/// A model sometimes uses a tool's own name where the session lists it with a
/// prefix (`observe` for `mcp_0_observe`), or a prefix from another session.
/// Nothing is resolved on its behalf: the reply names the listed tools that
/// differ only by that prefix, so the next call can be exact. The requested
/// name is model text and is not repeated.
pub fn unlisted(requested: &str, definitions: &[Value]) -> String {
    let own = own_name(requested);
    let mut close: Vec<&str> = definitions
        .iter()
        .filter_map(|definition| definition["name"].as_str())
        .filter(|listed| own_name(listed) == own)
        .collect();
    close.sort_unstable();
    close.truncate(5);
    let hint = if close.is_empty() {
        String::new()
    } else {
        format!(" It is listed here as: {}.", close.join(", "))
    };
    format!(
        "No tool with that name is listed in this session, so nothing was run.{hint} Call a tool by the exact name it is listed under."
    )
}

/// A listed MCP tool is `mcp_<index>_<name>`; any other name is its own.
fn own_name(name: &str) -> &str {
    name.strip_prefix("mcp_")
        .and_then(|rest| rest.split_once('_'))
        .filter(|(index, tool)| {
            !index.is_empty() && index.bytes().all(|b| b.is_ascii_digit()) && !tool.is_empty()
        })
        .map_or(name, |(_, tool)| tool)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listed(names: &[&str]) -> Vec<Value> {
        names.iter().map(|name| json!({"name":name})).collect()
    }

    #[test]
    fn an_unlisted_name_is_answered_with_the_listed_names_that_differ_by_prefix() {
        let definitions = listed(&["clock", "mcp_0_context_read", "mcp_1_mind_note"]);
        let exact = "Call a tool by the exact name it is listed under.";
        // The tool's own name, and a prefix carried over from another session.
        for requested in ["mind_note", "mcp_0_mind_note", "mcp_7_mind_note"] {
            let reply = unlisted(requested, &definitions);
            assert!(
                reply.contains("It is listed here as: mcp_1_mind_note."),
                "{reply}"
            );
            assert!(reply.contains("nothing was run") && reply.ends_with(exact));
            assert!(!reply.contains("mcp_0_context_read") && !reply.contains("clock"));
        }
        // A built-in asked for with a prefix it does not have.
        assert!(unlisted("mcp_2_clock", &definitions).contains("listed here as: clock."));
        // The same tool on two servers: both are named, neither is chosen.
        let twice = listed(&["mcp_1_observe", "mcp_0_observe"]);
        assert!(unlisted("observe", &twice).contains("as: mcp_0_observe, mcp_1_observe."));
    }

    #[test]
    fn an_unlisted_name_is_never_repeated_and_nothing_is_guessed() {
        let definitions = listed(&["clock", "mcp_0_mind_note"]);
        let requested = "PRIVATE-NAME-DO-NOT-ECHO; rm -rf";
        let reply = unlisted(requested, &definitions);
        assert!(!reply.contains(requested) && !reply.contains("PRIVATE"));
        assert!(!reply.contains("listed here as"), "{reply}");
        // A near miss is not a match: only the prefix may differ.
        for requested in [
            "mind_notes",
            "note",
            "mcp__mind_note",
            "mcp_x_mind_note",
            "",
        ] {
            assert!(
                !unlisted(requested, &definitions).contains("listed here as"),
                "{requested}"
            );
        }
        // At most five names, so a reply stays short.
        let many: Vec<String> = (0..9).map(|i| format!("mcp_{i}_observe")).collect();
        let many = listed(&many.iter().map(String::as_str).collect::<Vec<_>>());
        assert_eq!(unlisted("observe", &many).matches("mcp_").count(), 5);
    }

    #[test]
    fn arguments_cannot_supply_resolved_identity() {
        let input = json!({"server":"minecraft","tool":"observe",
            "_meta":{"com.microsoft.amplifier.tool":{"version":1,"kind":"mcp",
                "server":"minecraft","name":"observe"}}});
        let builtin =
            Identity::Builtin("workspace_write").call("a", "workspace write", "edit", &input);
        assert_eq!(builtin["rawInput"], input);
        assert_eq!(
            builtin["_meta"][META],
            json!({"version":1,"kind":"builtin","name":"workspace_write"})
        );
        let mcp = Identity::Mcp {
            server: "other",
            name: "write",
        }
        .call("b", "other / write", "edit", &input);
        assert_eq!(
            mcp["_meta"][META],
            json!({"version":1,"kind":"mcp","server":"other","name":"write"})
        );
        assert_eq!(mcp["toolCallId"], "b");
    }
}
