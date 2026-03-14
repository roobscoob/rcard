use std::collections::HashMap;

use serde::Deserialize;

#[allow(unused)]
#[derive(Debug, Deserialize)]
pub struct LogMetadataFile {
    #[serde(default)]
    pub task_names: Vec<String>,
    #[serde(default)]
    pub types: HashMap<String, serde_json::Value>,
    #[serde(default)]
    pub fields: HashMap<String, serde_json::Value>,
    #[serde(default)]
    pub species: HashMap<String, Species>,
}

#[allow(unused)]
#[derive(Debug, Deserialize)]
pub struct Species {
    pub format: String,
    pub arg_count: u32,
    pub kind: String,
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub line: Option<u32>,
    #[serde(default)]
    pub column: Option<u32>,
    #[serde(default)]
    pub end_line: Option<u32>,
    #[serde(default)]
    pub end_column: Option<u32>,
}
