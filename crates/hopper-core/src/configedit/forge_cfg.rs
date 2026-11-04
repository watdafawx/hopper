//! (Being written.)

use super::{Section, Value};

pub(super) fn parse(_text: &str) -> Result<Vec<Section>, String> {
    Err("Hopper can't read this kind of file as settings yet.".into())
}

pub(super) fn set(_text: &str, _path: &[String], _key: &str, _v: &Value) -> Result<String, String> {
    Err("Hopper can't edit this kind of file as settings yet.".into())
}
