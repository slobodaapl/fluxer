// SPDX-License-Identifier: AGPL-3.0-or-later

use napi::Task;
use napi::bindgen_prelude::{AsyncTask, Env, Error, Result, Status};
use napi_derive::napi;

use crate::ShellError;

impl From<ShellError> for Error {
    fn from(error: ShellError) -> Self {
        match error {
            ShellError::InvalidArg(message) => Self::new(Status::InvalidArg, message),
            ShellError::Generic(message) => Self::new(Status::GenericFailure, message),
        }
    }
}

#[napi(object)]
#[derive(Debug)]
pub struct CreateShortcutOptions {
    #[napi(js_name = "lnkPath")]
    pub lnk_path: Option<String>,
    pub target: Option<String>,
    pub args: Option<String>,
    #[napi(js_name = "appUserModelId")]
    pub app_user_model_id: Option<String>,
    #[napi(js_name = "toastActivatorClsid")]
    pub toast_activator_clsid: Option<String>,
    #[napi(js_name = "iconPath")]
    pub icon_path: Option<String>,
    #[napi(js_name = "iconIndex")]
    pub icon_index: Option<i32>,
    #[napi(js_name = "workingDir")]
    pub working_dir: Option<String>,
    pub description: Option<String>,
}

#[napi(object)]
#[derive(Debug)]
pub struct SetRunValueOptions {
    pub name: Option<String>,
    pub command: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RunValueOptions {
    name: String,
    command: String,
}

pub struct CreateShortcutTask {
    options: crate::ShortcutOptions,
}

pub struct SetRunValueTask {
    options: RunValueOptions,
}

pub struct DeleteRunValueTask {
    name: String,
}

pub struct GetRunValueTask {
    name: String,
}

#[napi(js_name = "createShortcut")]
pub fn create_shortcut(opts: CreateShortcutOptions) -> Result<AsyncTask<CreateShortcutTask>> {
    Ok(AsyncTask::new(CreateShortcutTask {
        options: validate_options(opts)?,
    }))
}

#[napi(js_name = "setCurrentUserRunValue")]
pub fn set_current_user_run_value(opts: SetRunValueOptions) -> Result<AsyncTask<SetRunValueTask>> {
    Ok(AsyncTask::new(SetRunValueTask {
        options: validate_run_value_options(opts)?,
    }))
}

#[napi(js_name = "deleteCurrentUserRunValue")]
pub fn delete_current_user_run_value(name: String) -> Result<AsyncTask<DeleteRunValueTask>> {
    Ok(AsyncTask::new(DeleteRunValueTask {
        name: require_non_empty(Some(name), "name")?,
    }))
}

#[napi(js_name = "getCurrentUserRunValue")]
pub fn get_current_user_run_value(name: String) -> Result<AsyncTask<GetRunValueTask>> {
    Ok(AsyncTask::new(GetRunValueTask {
        name: require_non_empty(Some(name), "name")?,
    }))
}

#[cfg(target_os = "windows")]
#[napi(js_name = "getUserNotificationState")]
pub fn get_user_notification_state() -> Result<String> {
    Ok(crate::get_user_notification_state()?)
}

impl Task for CreateShortcutTask {
    type Output = ();
    type JsValue = ();

    fn compute(&mut self) -> Result<Self::Output> {
        Ok(crate::create_shortcut(&self.options)?)
    }

    fn resolve(&mut self, _env: Env, _output: Self::Output) -> Result<Self::JsValue> {
        Ok(())
    }
}

impl Task for SetRunValueTask {
    type Output = ();
    type JsValue = ();

    fn compute(&mut self) -> Result<Self::Output> {
        Ok(crate::set_current_user_run_value(
            &self.options.name,
            &self.options.command,
        )?)
    }

    fn resolve(&mut self, _env: Env, _output: Self::Output) -> Result<Self::JsValue> {
        Ok(())
    }
}

impl Task for DeleteRunValueTask {
    type Output = ();
    type JsValue = ();

    fn compute(&mut self) -> Result<Self::Output> {
        Ok(crate::delete_current_user_run_value(&self.name)?)
    }

    fn resolve(&mut self, _env: Env, _output: Self::Output) -> Result<Self::JsValue> {
        Ok(())
    }
}

impl Task for GetRunValueTask {
    type Output = Option<String>;
    type JsValue = Option<String>;

    fn compute(&mut self) -> Result<Self::Output> {
        Ok(crate::get_current_user_run_value(&self.name)?)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

fn validate_options(opts: CreateShortcutOptions) -> Result<crate::ShortcutOptions> {
    Ok(crate::ShortcutOptions {
        lnk_path: require_non_empty(opts.lnk_path, "lnkPath")?,
        target: require_non_empty(opts.target, "target")?,
        args: opts.args,
        app_user_model_id: opts.app_user_model_id,
        toast_activator_clsid: opts.toast_activator_clsid,
        icon_path: opts.icon_path,
        icon_index: opts.icon_index.unwrap_or(0),
        working_dir: opts.working_dir,
        description: opts.description,
    })
}

fn validate_run_value_options(opts: SetRunValueOptions) -> Result<RunValueOptions> {
    Ok(RunValueOptions {
        name: require_non_empty(opts.name, "name")?,
        command: require_non_empty(opts.command, "command")?,
    })
}

fn require_non_empty(value: Option<String>, property: &'static str) -> Result<String> {
    let value = value.unwrap_or_default();
    crate::require_non_empty(&value, property)?;
    Ok(value)
}
