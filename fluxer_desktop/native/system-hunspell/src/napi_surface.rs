// SPDX-License-Identifier: AGPL-3.0-or-later

use std::path::Path;

use crate::HunspellError;
use napi::bindgen_prelude::{Error, Result, Status};
use napi_derive::napi;

impl From<HunspellError> for Error {
    fn from(error: HunspellError) -> Self {
        let status = match error {
            HunspellError::InvalidArgument(_) => Status::InvalidArg,
            HunspellError::Load(_) => Status::GenericFailure,
        };
        Self::new(status, error.to_string())
    }
}

#[napi(object)]
pub struct SystemDictionary {
    pub tag: String,
    #[napi(js_name = "affPath")]
    pub aff_path: String,
    #[napi(js_name = "dicPath")]
    pub dic_path: String,
}

#[napi]
pub struct Hunspell {
    inner: crate::Hunspell,
}

#[napi]
impl Hunspell {
    #[napi(constructor)]
    pub fn new(aff_path: String, dic_path: String) -> Result<Self> {
        Ok(Self {
            inner: crate::Hunspell::new(Path::new(&aff_path), Path::new(&dic_path))?,
        })
    }

    #[napi]
    pub fn spell(&self, word: String) -> Result<bool> {
        Ok(self.inner.spell(&word)?)
    }

    #[napi]
    pub fn suggest(&self, word: String, max: Option<u32>) -> Result<Vec<String>> {
        Ok(self.inner.suggest(&word, max)?)
    }

    #[napi]
    pub fn add(&mut self, word: String) -> Result<()> {
        Ok(self.inner.add(&word)?)
    }

    #[napi]
    pub fn remove(&mut self, word: String) -> Result<()> {
        Ok(self.inner.remove(&word)?)
    }

    #[napi]
    pub fn close(&mut self) {
        self.inner.close();
    }
}

#[napi(js_name = "discoverSystemDictionaries")]
pub fn discover_system_dictionaries() -> Vec<SystemDictionary> {
    crate::discover_system_dictionaries()
        .into_iter()
        .map(|dict| SystemDictionary {
            tag: dict.tag,
            aff_path: dict.aff_path.display().to_string(),
            dic_path: dict.dic_path.display().to_string(),
        })
        .collect()
}
