//! Explicit host-projected task limits. Absence or uncertainty is not unlimited
//! authority; only a deliberately configured variant represents no task limit.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Bound<T> {
    Finite { value: T },
    NoConfiguredBound {},
}

impl<T: Copy + Ord> Bound<T> {
    pub fn permits_at_least(&self, minimum: T) -> bool {
        match self {
            Self::Finite { value } => *value >= minimum,
            Self::NoConfiguredBound {} => true,
        }
    }

    pub fn cap(&self, value: T) -> T {
        match self {
            Self::Finite { value: limit } => value.min(*limit),
            Self::NoConfiguredBound {} => value,
        }
    }
}

/// Static local CLI discovery: no installation, credentials, ACP connection or
/// model call is needed. This does not advertise an agent's task authority.
pub fn helper_capabilities() -> Value {
    json!({
        "schema_version": 1,
        "runtime_version": env!("CARGO_PKG_VERSION"),
        "helpers": {
            "watch": {
                "version": 2,
                "command": "watch-v2",
                "legacy_command": "watch",
                "bound_kinds": ["finite", "no_configured_bound"],
                "watch_ttl_seconds": {"minimum": 30, "maximum": 1800}
            },
            "continuation": {
                "version": 2,
                "command": "continuation-v2",
                "legacy_command": "continuation",
                "bound_kinds": ["finite", "no_configured_bound"]
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absence_unknown_and_contradictory_bounds_are_not_unlimited() {
        for value in [
            json!(null),
            json!(0),
            json!({}),
            json!({"kind":"unknown"}),
            json!({"kind":"finite"}),
            json!({"kind":"finite","value":-1}),
            json!({"kind":"finite","value":1.5}),
            json!({"kind":"no_configured_bound","value":1}),
        ] {
            assert!(
                serde_json::from_value::<Bound<u64>>(value.clone()).is_err(),
                "{value}"
            );
        }
        let zero: Bound<u32> = serde_json::from_value(json!({"kind":"finite","value":0})).unwrap();
        assert!(!zero.permits_at_least(1));
        let none: Bound<u32> =
            serde_json::from_value(json!({"kind":"no_configured_bound"})).unwrap();
        assert!(none.permits_at_least(1));
        assert_eq!(none.cap(30), 30);
    }
}
