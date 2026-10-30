// SPDX-License-Identifier: AGPL-3.0-or-later

mod dictionaries;
mod encoding;
#[cfg(feature = "napi")]
pub mod napi_surface;

use std::ffi::{CStr, CString};
use std::fs::File;
use std::path::Path;
use std::ptr;
use std::sync::{Mutex, MutexGuard};

use crate::encoding::is_utf8_encoding;

pub use crate::dictionaries::{EnvSnapshot, SystemDictionary, discover_dictionaries};

static HUNSPELL_LOCK: Mutex<()> = Mutex::new(());

/// Why a call failed. The napi surface maps `InvalidArgument` to `Status::InvalidArg` and
/// `Load` to `Status::GenericFailure`, with `Display` as the message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HunspellError {
    InvalidArgument(String),
    Load(&'static str),
}

impl std::fmt::Display for HunspellError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidArgument(message) => f.write_str(message),
            Self::Load(reason) => write!(f, "Hunspell load failed: {reason}"),
        }
    }
}

impl std::error::Error for HunspellError {}

pub type Result<T> = std::result::Result<T, HunspellError>;

/// A UTF-8 Hunspell dictionary. `close` drops it; a closed instance spells nothing, suggests
/// nothing and ignores additions and removals.
#[derive(Debug)]
pub struct Hunspell {
    dict: Option<Dictionary>,
}

impl Hunspell {
    pub fn new(aff_path: &Path, dic_path: &Path) -> Result<Self> {
        Ok(Self {
            dict: Some(Dictionary::load(aff_path, dic_path)?),
        })
    }

    pub fn spell(&self, word: &str) -> Result<bool> {
        validate_text_arg(word, "word")?;
        Ok(self
            .dict
            .as_ref()
            .is_some_and(|dict| dict.spell(word).unwrap_or(true)))
    }

    /// At most `max` suggestions; a `max` outside 1..=64 (or none) means 8.
    pub fn suggest(&self, word: &str, max: Option<u32>) -> Result<Vec<String>> {
        validate_text_arg(word, "word")?;
        let Some(dict) = &self.dict else {
            return Ok(Vec::new());
        };
        let limit = normalize_suggestion_limit(max);
        dict.suggest(word, limit)
    }

    /// Adds `word` to the runtime word list. A Hunspell refusal is ignored.
    pub fn add(&mut self, word: &str) -> Result<()> {
        validate_text_arg(word, "word")?;
        if let Some(dict) = &mut self.dict {
            let _ = dict.add(word);
        }
        Ok(())
    }

    pub fn remove(&mut self, word: &str) -> Result<()> {
        validate_text_arg(word, "word")?;
        if let Some(dict) = &mut self.dict {
            let _ = dict.remove(word);
        }
        Ok(())
    }

    pub fn close(&mut self) {
        self.dict = None;
    }
}

/// The dictionaries installed on this system, from the `HUNSPELL_DICT_DIR`, `XDG_DATA_HOME`,
/// `HOME` and `XDG_DATA_DIRS` of the process environment.
pub fn discover_system_dictionaries() -> Vec<SystemDictionary> {
    let snapshot = EnvSnapshot {
        hunspell_dict_dir: std::env::var("HUNSPELL_DICT_DIR").ok(),
        xdg_data_home: std::env::var("XDG_DATA_HOME").ok(),
        home: std::env::var("HOME").ok(),
        xdg_data_dirs: std::env::var("XDG_DATA_DIRS").ok(),
    };
    discover_dictionaries(&snapshot)
}

#[derive(Debug)]
struct Dictionary {
    handle: *mut hunspell_sys::Hunhandle,
}

// SAFETY: a Hunspell handle has no thread affinity, and every call on it goes through
// `HUNSPELL_LOCK`, so moving the owner to another thread is sound.
unsafe impl Send for Dictionary {}

impl Dictionary {
    fn load(aff_path: &Path, dic_path: &Path) -> Result<Self> {
        validate_path_arg(aff_path)?;
        validate_path_arg(dic_path)?;
        if File::open(aff_path).is_err() || File::open(dic_path).is_err() {
            return Err(HunspellError::Load("LoadFailed"));
        }

        let aff = cstring_arg(aff_path.as_os_str().as_encoded_bytes(), "affPath")?;
        let dic = cstring_arg(dic_path.as_os_str().as_encoded_bytes(), "dicPath")?;
        let _guard = hunspell_lock();

        let handle = unsafe { hunspell_sys::Hunspell_create(aff.as_ptr(), dic.as_ptr()) };
        if handle.is_null() {
            return Err(HunspellError::Load("LoadFailed"));
        }

        let encoding_ptr = unsafe { hunspell_sys::Hunspell_get_dic_encoding(handle) };
        if encoding_ptr.is_null() {
            unsafe { hunspell_sys::Hunspell_destroy(handle) };
            return Err(HunspellError::Load("LoadFailed"));
        }

        let encoding = unsafe { CStr::from_ptr(encoding_ptr) }
            .to_str()
            .map_err(|_| HunspellError::Load("DictionaryNotUtf8"))?;
        if !is_utf8_encoding(encoding) {
            unsafe { hunspell_sys::Hunspell_destroy(handle) };
            return Err(HunspellError::Load("DictionaryNotUtf8"));
        }

        Ok(Self { handle })
    }

    fn spell(&self, word: &str) -> Result<bool> {
        let word = cstring_arg(word.as_bytes(), "word")?;
        let _guard = hunspell_lock();

        Ok(unsafe { hunspell_sys::Hunspell_spell(self.handle, word.as_ptr()) } != 0)
    }

    fn suggest(&self, word: &str, max: usize) -> Result<Vec<String>> {
        let word = cstring_arg(word.as_bytes(), "word")?;
        let mut raw: *mut *mut std::os::raw::c_char = ptr::null_mut();
        let _hunspell_guard = hunspell_lock();

        let count = unsafe { hunspell_sys::Hunspell_suggest(self.handle, &mut raw, word.as_ptr()) };
        let _guard = SuggestionList {
            handle: self.handle,
            raw,
            count,
        };
        if count <= 0 || raw.is_null() {
            return Ok(Vec::new());
        }
        let limit = (count as usize).min(max);
        let mut out = Vec::with_capacity(limit);
        for index in 0..limit {
            let item = unsafe { *raw.add(index) };
            if item.is_null() {
                out.push(String::new());
                continue;
            }

            let suggestion = unsafe { CStr::from_ptr(item) };
            if let Ok(text) = suggestion.to_str() {
                out.push(text.to_owned());
            }
        }
        Ok(out)
    }

    fn add(&mut self, word: &str) -> Result<()> {
        let word = cstring_arg(word.as_bytes(), "word")?;
        let _guard = hunspell_lock();

        let rc = unsafe { hunspell_sys::Hunspell_add(self.handle, word.as_ptr()) };
        if rc == 0 {
            Ok(())
        } else {
            Err(HunspellError::Load("LoadFailed"))
        }
    }

    fn remove(&mut self, word: &str) -> Result<()> {
        let word = cstring_arg(word.as_bytes(), "word")?;
        let _guard = hunspell_lock();

        unsafe { hunspell_sys::Hunspell_remove(self.handle, word.as_ptr()) };
        Ok(())
    }
}

impl Drop for Dictionary {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            let _guard = hunspell_lock();

            unsafe { hunspell_sys::Hunspell_destroy(self.handle) };
            self.handle = ptr::null_mut();
        }
    }
}

struct SuggestionList {
    handle: *mut hunspell_sys::Hunhandle,
    raw: *mut *mut std::os::raw::c_char,
    count: i32,
}

impl Drop for SuggestionList {
    fn drop(&mut self) {
        if self.count > 0 && !self.raw.is_null() {
            unsafe { hunspell_sys::Hunspell_free_list(self.handle, &mut self.raw, self.count) };
        }
    }
}

fn normalize_suggestion_limit(max: Option<u32>) -> usize {
    match max {
        Some(value @ 1..=64) => value as usize,
        _ => 8,
    }
}

fn hunspell_lock() -> MutexGuard<'static, ()> {
    HUNSPELL_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn validate_path_arg(path: &Path) -> Result<()> {
    let bytes = path.as_os_str().as_encoded_bytes();
    if bytes.is_empty() {
        return Err(HunspellError::InvalidArgument(
            "path must be non-empty".to_owned(),
        ));
    }
    if bytes.contains(&0) {
        return Err(HunspellError::InvalidArgument(
            "path must not contain NUL bytes".to_owned(),
        ));
    }
    Ok(())
}

fn validate_text_arg(value: &str, label: &str) -> Result<()> {
    if value.as_bytes().contains(&0) {
        return Err(HunspellError::InvalidArgument(format!(
            "{label} must not contain NUL bytes"
        )));
    }
    Ok(())
}

fn cstring_arg(value: &[u8], label: &str) -> Result<CString> {
    CString::new(value)
        .map_err(|_| HunspellError::InvalidArgument(format!("{label} must not contain NUL bytes")))
}
