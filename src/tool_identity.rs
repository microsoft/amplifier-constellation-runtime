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

#[cfg(test)]
mod tests {
    use super::*;

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
