//! Parameters: the JSON object a run hands its kernel (#650), read by
//! `ganymede execute -p` and `POST /api/notebooks/run` alike. The kernel
//! gets it in [`PARAMS_ENV`]; the executed notebook records it in
//! `metadata.ganymede.params`. The crate knows no kernel language: a
//! parameter is data in an environment variable.

use serde_json::{Map, Value};

use crate::Notebook;

/// The environment variable that carries a run's parameters to the
/// kernel: a JSON object. cdp-kernel reads it as `jupyter.params`.
pub const PARAMS_ENV: &str = "GANYMEDE_PARAMS";

/// Largest [`PARAMS_ENV`] value, in bytes: Windows caps a variable at
/// 32,767 characters, and Linux an argument or variable at 128 KiB.
pub const PARAMS_MAX: usize = 32_767;

/// `params` as the value of [`PARAMS_ENV`]; an error past [`PARAMS_MAX`].
pub fn params_env(params: &Map<String, Value>) -> Result<String, String> {
    let text = Value::Object(params.clone()).to_string();
    if text.len() > PARAMS_MAX {
        return Err(format!(
            "parameters are {} bytes as JSON, over the {PARAMS_MAX}-byte limit; pass a path to a file instead",
            text.len()
        ));
    }
    Ok(text)
}

/// Records a run's parameters in `metadata.ganymede.params`, so the
/// executed notebook says what it ran with; a run without them (`None`)
/// removes an earlier run's.
pub fn record_params(nb: &mut Notebook, params: Option<&Map<String, Value>>) {
    let Some(params) = params else {
        if let Some(g) = nb
            .metadata
            .get_mut("ganymede")
            .and_then(|g| g.as_object_mut())
        {
            if g.remove("params").is_some() && g.is_empty() {
                nb.metadata.remove("ganymede");
            }
        }
        return;
    };
    let ganymede = nb
        .metadata
        .entry("ganymede")
        .or_insert_with(|| Value::Object(Default::default()));
    if !ganymede.is_object() {
        *ganymede = Value::Object(Default::default());
    }
    ganymede["params"] = Value::Object(params.clone());
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn obj(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn env_is_compact_json() {
        assert_eq!(
            params_env(&obj(json!({"a": 1, "b": "x"}))).unwrap(),
            r#"{"a":1,"b":"x"}"#
        );
        assert_eq!(params_env(&Map::new()).unwrap(), "{}");
    }

    #[test]
    fn env_refuses_past_the_limit() {
        let big = obj(json!({"a": "x".repeat(PARAMS_MAX)}));
        assert!(params_env(&big).unwrap_err().contains("limit"));
    }

    #[test]
    fn records_and_removes() {
        let mut nb = Notebook::from_str(
            r#"{"cells":[],"metadata":{"ganymede":{"other":1}},"nbformat":4,"nbformat_minor":5}"#,
        )
        .unwrap();
        record_params(&mut nb, Some(&obj(json!({"day": "mon"}))));
        assert_eq!(
            nb.metadata["ganymede"],
            json!({"other": 1, "params": {"day": "mon"}})
        );
        record_params(&mut nb, None);
        assert_eq!(nb.metadata["ganymede"], json!({"other": 1}));

        let mut nb = Notebook::from_str(
            r#"{"cells":[],"metadata":{"ganymede":"bad"},"nbformat":4,"nbformat_minor":5}"#,
        )
        .unwrap();
        record_params(&mut nb, Some(&Map::new()));
        assert_eq!(nb.metadata["ganymede"], json!({"params": {}}));
        record_params(&mut nb, None);
        assert!(!nb.metadata.contains_key("ganymede"));
    }
}
