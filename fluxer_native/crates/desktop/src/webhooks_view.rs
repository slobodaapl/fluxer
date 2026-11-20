use super::*;
#[derive(Clone)]
enum Form {
    Create,
    Rename(String),
    Delete(String, String),
}

pub(super) struct WebhooksUi {
    pub channel: Option<Snowflake>,
    pub public_endpoint: Option<String>,
    form: Option<Form>,
    name: Entity<TextInput>,
    mutating: bool,
    request: u64,
    rows: Vec<fluxer_protocol::Webhook>,
    pending: bool,
    error: Option<String>,
    focus: FocusHandle,
    previous: Option<FocusHandle>,
}
impl WebhooksUi {
    pub fn reset(&mut self) {
        self.form = None;
        self.mutating = false;
        self.channel = None;
        self.rows.clear();
        self.error = None;
        self.pending = false;
        self.previous = None;
        self.request = self.request.wrapping_add(1);
    }
    pub fn new(cx: &mut Context<Fluxer>) -> Self {
        Self {
            channel: None,
            form: None,
            name: cx.new(|cx| TextInput::new("Webhook name", cx)),
            mutating: false,
            public_endpoint: None,
            request: 0,
            rows: vec![],
            pending: false,
            error: None,
            focus: cx.focus_handle(),
            previous: None,
        }
    }
}
impl Fluxer {
    pub(super) fn open_webhooks(
        &mut self,
        channel: Snowflake,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.webhooks_ui.form = None;
        self.webhooks_ui.mutating = false;
        self.webhooks_ui.channel = Some(channel);
        self.webhooks_ui.rows.clear();
        self.webhooks_ui.previous = window.focused(cx);
        self.webhooks_ui.focus.focus(window);
        self.fetch_webhooks(cx);
    }
    fn fetch_webhooks(&mut self, cx: &mut Context<Self>) {
        let Some(channel_id) = self.webhooks_ui.channel else {
            return;
        };
        self.webhooks_ui.request = self.webhooks_ui.request.wrapping_add(1);
        self.webhooks_ui.pending = true;
        self.webhooks_ui.error = None;
        let result = self
            .controller
            .as_ref()
            .ok_or_else(|| "The native client worker is unavailable".to_owned())
            .and_then(|c| {
                c.send(Command::FetchWebhooks {
                    request_id: self.webhooks_ui.request,
                    channel_id,
                })
            });
        if let Err(error) = result {
            self.webhooks_ui.pending = false;
            self.webhooks_ui.error = Some(error);
        }
        cx.notify();
    }
    fn close_webhooks(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.webhooks_ui.mutating {
            return;
        }
        self.command(Command::CancelWebhooks, cx);
        self.webhooks_ui.channel = None;
        self.webhooks_ui.rows.clear();
        self.webhooks_ui.request = self.webhooks_ui.request.wrapping_add(1);
        if let Some(focus) = self.webhooks_ui.previous.take() {
            focus.focus(window);
        }
        cx.notify();
    }
    pub(super) fn webhooks_event(
        &mut self,
        request: u64,
        channel: Snowflake,
        result: Result<Vec<fluxer_protocol::Webhook>, String>,
        cx: &mut Context<Self>,
    ) {
        if self.webhooks_ui.channel != Some(channel) || self.webhooks_ui.request != request {
            return;
        }
        self.webhooks_ui.pending = false;
        self.webhooks_ui.mutating = false;
        match result {
            Ok(rows) => {
                self.webhooks_ui.form = None;
                self.webhooks_ui.rows = rows;
                self.webhooks_ui.error = None;
            }
            Err(error) => {
                self.webhooks_ui.form = None;
                self.webhooks_ui.rows.clear();
                self.webhooks_ui.error = Some(error);
            }
        }
        cx.notify();
    }
    fn begin_webhook_form(
        &mut self,
        form: Form,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.webhooks_ui
            .name
            .update(cx, |input, cx| input.set(name, cx));
        let edits = !matches!(form, Form::Delete(..));
        self.webhooks_ui.form = Some(form);
        self.webhooks_ui.error = None;
        if edits {
            window.focus(&self.webhooks_ui.name.focus_handle(cx));
        }
        cx.notify();
    }
    fn submit_webhook(&mut self, cx: &mut Context<Self>) {
        if self.webhooks_ui.pending {
            return;
        }
        let Some(channel_id) = self.webhooks_ui.channel else {
            return;
        };
        let name = self.webhooks_ui.name.read(cx).content.to_string();
        let action = match self.webhooks_ui.form.clone() {
            Some(Form::Create) if !name.trim().is_empty() && name.chars().count() <= 80 => {
                fluxer_client::WebhookAction::Create { name }
            }
            Some(Form::Rename(id)) if !name.trim().is_empty() && name.chars().count() <= 80 => {
                fluxer_client::WebhookAction::Rename { id, name }
            }
            Some(Form::Delete(id, _)) => fluxer_client::WebhookAction::Delete { id },
            Some(_) => {
                self.webhooks_ui.error =
                    Some("Enter a webhook name between 1 and 80 characters.".into());
                cx.notify();
                return;
            }
            None => return,
        };
        self.webhooks_ui.request = self.webhooks_ui.request.wrapping_add(1);
        self.webhooks_ui.pending = true;
        self.webhooks_ui.mutating = true;
        self.webhooks_ui.error = None;
        let result = self
            .controller
            .as_ref()
            .ok_or_else(|| "The native client worker is unavailable".to_owned())
            .and_then(|c| {
                c.send(Command::MutateWebhook {
                    request_id: self.webhooks_ui.request,
                    channel_id,
                    action,
                })
            });
        if let Err(error) = result {
            self.webhooks_ui.pending = false;
            self.webhooks_ui.mutating = false;
            self.webhooks_ui.error = Some(error);
        }
        cx.notify();
    }
    pub(super) fn webhooks_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.keystroke.key == "escape" && !self.webhooks_ui.pending {
            if self.webhooks_ui.form.take().is_some() {
                self.webhooks_ui.error = None;
                self.webhooks_ui.focus.focus(window);
                cx.notify();
            } else {
                self.close_webhooks(window, cx);
            }
            cx.stop_propagation();
        } else if !self.webhooks_ui.name.focus_handle(cx).is_focused(window) {
            cx.stop_propagation();
        }
    }
    pub(super) fn webhooks_dialog(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut list = div()
            .id("webhooks-list")
            .h(px(240.))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_1();
        for (index, row) in self.webhooks_ui.rows.iter().enumerate() {
            let mut buttons = div().flex().gap_1();
            if self.webhooks_ui.public_endpoint.is_some() && row.has_token() {
                buttons = buttons.child(
                    crate::controls::quiet_button(("copy-webhook", index), "Copy URL").on_click(
                        cx.listener(move |this, _, _, cx| {
                            if let (Some(row), Some(base)) = (
                                this.webhooks_ui.rows.get(index),
                                this.webhooks_ui.public_endpoint.as_ref(),
                            ) {
                                if let Some(url) = row.copy_url(base) {
                                    cx.write_to_clipboard(ClipboardItem::new_string(url));
                                }
                            }
                        }),
                    ),
                );
            }
            let id = row.id.clone();
            let name = row.name.clone();
            buttons = buttons.child(
                crate::controls::quiet_button(("rename-webhook", index), "Rename").on_click(
                    cx.listener(move |this, _, window, cx| {
                        this.begin_webhook_form(Form::Rename(id.clone()), name.clone(), window, cx)
                    }),
                ),
            );
            let id = row.id.clone();
            let name = row.name.clone();
            buttons = buttons.child(
                crate::controls::quiet_button(("delete-webhook", index), "Delete").on_click(
                    cx.listener(move |this, _, window, cx| {
                        this.begin_webhook_form(
                            Form::Delete(id.clone(), name.clone()),
                            String::new(),
                            window,
                            cx,
                        )
                    }),
                ),
            );
            list = list.child(
                div()
                    .p_2()
                    .rounded_md()
                    .bg(rgb(0x303138))
                    .flex()
                    .justify_between()
                    .child(row.name.clone())
                    .child(buttons),
            );
        }
        let mut panel = div()
            .id("webhooks-dialog")
            .track_focus(&self.webhooks_ui.focus)
            .w(px(480.))
            .p_5()
            .rounded_lg()
            .bg(rgb(PANEL))
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .text_size(px(20.))
                    .font_weight(FontWeight::BOLD)
                    .child("Channel webhooks"),
            );
        if self.webhooks_ui.pending {
            panel = panel.child("Working…");
        } else if let Some(form) = &self.webhooks_ui.form {
            panel = match form {
                Form::Create => panel
                    .child("Create webhook")
                    .child(self.webhooks_ui.name.clone()),
                Form::Rename(_) => panel
                    .child("Rename webhook")
                    .child(self.webhooks_ui.name.clone()),
                Form::Delete(_, name) => {
                    panel.child(format!("Delete {name}? This cannot be undone."))
                }
            };
            if let Some(error) = &self.webhooks_ui.error {
                panel = panel.child(div().text_color(rgb(0xe15059)).child(error.clone()));
            }
            panel = panel.child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        crate::controls::quiet_button("webhook-cancel", "Cancel").on_click(
                            cx.listener(|this, _, window, cx| {
                                this.webhooks_ui.form = None;
                                this.webhooks_ui.error = None;
                                this.webhooks_ui.focus.focus(window);
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        crate::controls::quiet_button(
                            "webhook-submit",
                            if matches!(form, Form::Delete(..)) {
                                "Confirm delete"
                            } else {
                                "Save"
                            },
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.submit_webhook(cx))),
                    ),
            );
        } else {
            if let Some(error) = &self.webhooks_ui.error {
                panel = panel
                    .child(div().text_color(rgb(0xe15059)).child(error.clone()))
                    .child(
                        crate::controls::quiet_button("webhooks-retry", "Refresh")
                            .on_click(cx.listener(|this, _, _, cx| this.fetch_webhooks(cx))),
                    );
            } else if self.webhooks_ui.rows.is_empty() {
                panel = panel.child("No webhooks to show.");
            } else {
                panel = panel.child(list);
            }
            let mut buttons = div().flex().justify_end().gap_2();
            if self.webhooks_ui.error.is_none() {
                buttons = buttons.child(
                    crate::controls::quiet_button("webhook-create", "Create webhook").on_click(
                        cx.listener(|this, _, window, cx| {
                            this.begin_webhook_form(Form::Create, String::new(), window, cx)
                        }),
                    ),
                );
            }
            panel =
                panel.child(buttons.child(
                    crate::controls::quiet_button("webhooks-close", "Close").on_click(
                        cx.listener(|this, _, window, cx| this.close_webhooks(window, cx)),
                    ),
                ));
        }
        div()
            .absolute()
            .inset_0()
            .bg(rgba(0x00000099))
            .occlude()
            .flex()
            .items_center()
            .justify_center()
            .child(panel)
            .into_any_element()
    }
}
