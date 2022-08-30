/*!
Defining the [Namespace] struct for handling namespaces in the catalog.
*/

use anyhow::{anyhow, Result};
pub struct Namespace {
    levels: Vec<String>,
}

impl Namespace {
    pub fn try_new(levels: &[String]) -> Result<Self> {
        if levels.iter().any(|x| x.is_empty()) {
            Err(anyhow!(
                "Error: Cannot create a namespace with an empty entry."
            ))
        } else {
            Ok(Namespace {
                levels: levels.to_vec(),
            })
        }
    }
    pub fn empty() -> Self {
        Namespace { levels: vec![] }
    }
    pub fn levels(&self) -> &[String] {
        &self.levels
    }
    pub fn len(&self) -> usize {
        self.levels.len()
    }
}
