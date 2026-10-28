use crate::hooks::{load_config, run_hook, BranchInputs, HookError, HookRequest, ReviewHookData};
use serde::de::{value::MapAccessDeserializer, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};
use std::fmt;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Resolution {
    #[serde(deserialize_with = "branch_object")]
    branches: BranchInputs,
    #[serde(default)]
    metadata: Map<String, Value>,
}

// Derived struct deserialization also accepts arrays; this boundary requires an object.
fn branch_object<'de, D: Deserializer<'de>>(deserializer: D) -> Result<BranchInputs, D::Error> {
    struct BranchObject;
    impl<'de> Visitor<'de> for BranchObject {
        type Value = BranchInputs;
        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a branches object")
        }
        fn visit_map<M: MapAccess<'de>>(self, map: M) -> Result<Self::Value, M::Error> {
            BranchInputs::deserialize(MapAccessDeserializer::new(map))
        }
    }
    deserializer.deserialize_map(BranchObject)
}

fn parse_resolution(bytes: &[u8]) -> Result<Resolution, HookError> {
    let invalid = |error| HookError::new(format!("Invalid PR resolution hook output: {error}"));
    let text =
        std::str::from_utf8(bytes).map_err(|error| invalid(format!("not valid UTF-8: {error}")))?;
    if !text.trim_start().starts_with('{') {
        return Err(invalid("expected one JSON object".to_owned()));
    }
    let resolution: Resolution =
        serde_json::from_str(text).map_err(|error| invalid(error.to_string()))?;
    if resolution.branches.target.is_empty() || resolution.branches.source.is_empty() {
        return Err(invalid(
            "branches.target and branches.source must be nonempty strings".to_owned(),
        ));
    }
    Ok(resolution)
}

pub fn resolve_pr(identifier: &str, verbose: bool) -> Result<ReviewHookData, HookError> {
    let hook = load_config()?.pr_hook.ok_or_else(|| HookError::new("PR review requires a resolver. Configure [review_branch.pr_hook] with program and optional args in your user-wide .cresca/config.toml."))?;
    let request = HookRequest {
        pr: Some(identifier.to_owned()),
    };
    #[derive(Serialize)]
    struct Input<'a> {
        request: &'a HookRequest,
    }
    let output = run_hook(
        &hook,
        "PR resolution",
        &Input { request: &request },
        verbose,
    )?;
    let resolution = parse_resolution(&output)?;
    Ok(ReviewHookData {
        request,
        branches: resolution.branches,
        metadata: resolution.metadata,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_branch_spellings_and_arbitrary_metadata() {
        let resolution = parse_resolution(
            concat!(
                r#" {"branches":{"target":" main ","source":"refs/heads/topic"},"metadata":{"ticket":[null,true,42,{"request":"custom"}]}}"#,
                " \n"
            )
            .as_bytes(),
        )
        .unwrap();
        assert_eq!(resolution.branches.target, " main ");
        assert_eq!(resolution.metadata["ticket"][3]["request"], "custom");
        assert!(
            parse_resolution(br#"{"branches":{"target":"main","source":"topic"}}"#)
                .unwrap()
                .metadata
                .is_empty()
        );
    }

    #[test]
    fn rejects_invalid_output_envelopes() {
        for text in [
            "",
            "[]",
            "null",
            r#"[{"branches":{"target":"main","source":"topic"}}]"#,
            r#"{"branches":{"target":"main","source":"topic"}} {}"#,
            r#"{"branches":{"target":"main","source":"topic"},"request":{}}"#,
            r#"{"branches":{"target":"main","source":"topic"},"metadata":null}"#,
            r#"{"branches":{"target":"main","source":"topic"},"metadata":[]}"#,
            r#"{"branches":{"target":"main","source":"topic"},"metadata":{},"metadata":{}}"#,
            r#"{"branches":{"target":"main","source":"topic"},"branches":{"target":"main","source":"topic"}}"#,
            r#"{"branches":["main","topic"]}"#,
            r#"{"branches":{"target":"main","target":"other","source":"topic"}}"#,
            r#"{"branches":{"target":"main","source":"topic","extra":1}}"#,
            r#"{"branches":{"target":"main"}}"#,
            r#"{"branches":{"target":"","source":"topic"}}"#,
            r#"{"branches":{"target":42,"source":"topic"}}"#,
            r#"{"branches":{"target":"main","source":null}}"#,
            r#"{"branches":{"target":"main","source":""}}"#,
            r#"{"branches":{"target":"main","source":"topic","source":"other"}}"#,
            r#"{"branches":{"target":"main","source":"topic"},"metadata":"text"}"#,
            r#"{"branches":null}"#,
            r#"{"metadata":{}}"#,
        ] {
            assert!(parse_resolution(text.as_bytes()).is_err(), "{text}");
        }
        assert!(parse_resolution(&[0xff]).is_err());
    }
}
