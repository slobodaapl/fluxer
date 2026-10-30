// SPDX-License-Identifier: AGPL-3.0-or-later

//! The toast XML of a Windows notification: the generic template with the title, the body, the
//! icon as the app logo, and no audio (Electron's `silent: true`). The template Electron built
//! is assumed, not checked against its source.
//!
//! Text is escaped and characters XML 1.0 cannot carry are dropped, so a title can neither add
//! elements (an `<image>` would make the toast fetch a URL) nor make the document unloadable.

/// `icon` is a `file:` URI.
pub fn toast_xml(title: &str, body: &str, icon: Option<&str>) -> String {
    let image = icon
        .map(|icon| {
            format!(
                r#"<image placement="appLogoOverride" src="{}"/>"#,
                escape(icon)
            )
        })
        .unwrap_or_default();
    format!(
        r#"<toast><visual><binding template="ToastGeneric"><text>{}</text><text>{}</text>{image}</binding></visual><audio silent="true"/></toast>"#,
        escape(title),
        escape(body)
    )
}

fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            '\t' | '\n' | '\r' => escaped.push(character),
            '\u{0}'..='\u{1f}' | '\u{fffe}' | '\u{ffff}' => {}
            _ => escaped.push(character),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_cannot_add_elements() {
        assert_eq!(
            toast_xml(r#"<image src="https://x.test/"/>"#, "a & b\u{1}\nc", None),
            "<toast><visual><binding template=\"ToastGeneric\">\
             <text>&lt;image src=&quot;https://x.test/&quot;/&gt;</text><text>a &amp; b\nc</text>\
             </binding></visual><audio silent=\"true\"/></toast>"
        );
    }

    #[test]
    fn the_icon_is_the_app_logo() {
        assert_eq!(
            toast_xml("t", "b", Some("file:///C:/Users/a'b/icon.png")),
            "<toast><visual><binding template=\"ToastGeneric\"><text>t</text><text>b</text>\
             <image placement=\"appLogoOverride\" src=\"file:///C:/Users/a&apos;b/icon.png\"/>\
             </binding></visual><audio silent=\"true\"/></toast>"
        );
    }
}
