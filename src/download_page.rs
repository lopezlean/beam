use crate::{
    duration::format_duration,
    session::{ResolvedSendMode, SessionSnapshot},
};
use indicatif::HumanBytes;
use std::{borrow::Cow, time::{Duration, SystemTime}};

const TEMPLATE: &str = include_str!("templates/download_page.html");
const PROJECT_URL: &str = "https://github.com/lopezlean/beam";

pub fn render(snapshot: &SessionSnapshot) -> String {
    let mut page = TEMPLATE.to_string();
    let size_label = match snapshot.content_length {
        Some(size) => HumanBytes(size).to_string(),
        None => format!("{} input", HumanBytes(snapshot.input_size)),
    };
    let expiry_seconds = snapshot
        .expires_at
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs()
        .to_string();
    let page_title = format!("Beam · {}", snapshot.display_name);
    let lead = lead_text(snapshot);
    let state = if snapshot.once {
        "Burn after reading"
    } else {
        "Available until expiry"
    };

    replace(&mut page, "__PAGE_TITLE__", &escape_html(&page_title));
    replace(&mut page, "__PROJECT_URL__", PROJECT_URL);
    replace(&mut page, "__BADGE__", snapshot.mode.badge_label());
    replace(&mut page, "__DISPLAY_NAME__", &escape_html(&snapshot.display_name));
    replace(
        &mut page,
        "__MODE_LABEL__",
        &escape_html(mode_label(&snapshot.mode)),
    );
    replace(&mut page, "__LEAD__", &escape_html(&lead));
    replace(
        &mut page,
        "__TTL__",
        &escape_html(&format_duration(snapshot.remaining)),
    );
    replace(&mut page, "__STATE__", &escape_html(state));
    replace(&mut page, "__TOKEN__", &escape_attr(&snapshot.token));
    replace(&mut page, "__EXPIRY_SECONDS__", &expiry_seconds);
    replace(&mut page, "__ACTION_PANEL__", &render_action_panel(snapshot));
    replace(&mut page, "__SUMMARY_ROWS__", &render_rows(snapshot, &size_label));
    replace(&mut page, "__NOTICES__", &render_notices(snapshot));
    page
}

fn render_action_panel(snapshot: &SessionSnapshot) -> String {
    if snapshot.requires_pin {
        format!(
            r#"<section class="action-card action-card--hero"><form class="download-form" method="get" action="/download/{token}"><label class="form__label" for="pin">PIN</label><input class="form__input" id="pin" name="pin" placeholder="123456" inputmode="numeric" autocomplete="one-time-code" /><button class="form__button" type="submit" id="download-button">Download now</button></form><p class="action-card__hint">A PIN is required before the file can be downloaded.</p></section>"#,
            token = escape_attr(&snapshot.token),
        )
    } else {
        format!(
            r#"<section class="quick-action"><a class="quick-action__button" href="/download/{token}" id="download-button">Download now</a></section>"#,
            token = escape_attr(&snapshot.token),
        )
    }
}

fn render_rows(snapshot: &SessionSnapshot, size_label: &str) -> String {
    let mut rows = vec![
        row(
            "Payload",
            &format!("{} ({})", snapshot.display_name, snapshot.content_kind),
            false,
        ),
        row("Download", &snapshot.download_name, false),
        row("Size", size_label, false),
        row("Transport", &snapshot.transport_label, false),
        row("Security", security_label(snapshot), false),
        row(
            snapshot.primary_link_label,
            &snapshot.primary_link,
            true,
        ),
    ];

    if let Some((label, link)) = snapshot
        .secondary_link_label
        .zip(snapshot.secondary_link.as_ref())
    {
        rows.push(row(label, link, true));
    }

    rows.join("")
}

fn row(label: &str, value: &str, accent: bool) -> String {
    let value_class = if accent {
        "sheet__value sheet__value--accent"
    } else {
        "sheet__value"
    };
    format!(
        r#"<div class="sheet__row"><dt class="sheet__label">{label}</dt><dd class="{value_class}">{value}</dd></div>"#,
        label = escape_html(label),
        value = escape_html(value),
        value_class = value_class,
    )
}

fn render_notices(snapshot: &SessionSnapshot) -> String {
    let mut notices = Vec::new();

    if snapshot.requires_pin {
        notices.push(notice(
            "notice notice--accent",
            "This Beam session is protected by a secret URL and an additional PIN.",
        ));
    }

    for warning in &snapshot.warnings {
        notices.push(notice("notice", warning));
    }

    if snapshot.mode.is_local() {
        notices.push(notice(
            "notice notice--warning",
            "Local mode uses HTTP for convenience. Use --global or --pin for sensitive files.",
        ));
        notices.push(notice(
            "notice",
            "The encrypted LAN link uses a temporary self-signed certificate and may show a browser warning.",
        ));
    }

    if notices.is_empty() {
        String::new()
    } else {
        format!(
            r#"<section class="notice-stack" aria-label="Session notes">{}</section>"#,
            notices.join("")
        )
    }
}

fn notice(class_name: &str, message: &str) -> String {
    format!(
        r#"<div class="{class_name}">{message}</div>"#,
        class_name = class_name,
        message = escape_html(message),
    )
}

fn lead_text(snapshot: &SessionSnapshot) -> Cow<'static, str> {
    if snapshot.mode.is_local() {
        Cow::Borrowed(
            "Zero-install download for your local network. HTTP stays primary for convenience, with HTTPS available as an optional encrypted fallback.",
        )
    } else {
        Cow::Borrowed(
            "Zero-install download for any browser. Open the temporary Beam link, grab the file, and let the session expire on its own.",
        )
    }
}

fn mode_label(mode: &ResolvedSendMode) -> &'static str {
    match mode {
        ResolvedSendMode::Global { .. } => "Global default",
        ResolvedSendMode::Local { .. } => "Local mode",
    }
}

fn security_label(snapshot: &SessionSnapshot) -> &'static str {
    if snapshot.requires_pin {
        "token URL + PIN"
    } else {
        "token URL"
    }
}

fn replace(target: &mut String, placeholder: &str, value: &str) {
    *target = target.replace(placeholder, value);
}

fn escape_attr(value: &str) -> String {
    escape_html(value)
}

fn escape_html(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::render;
    use crate::{
        provider::ProviderKind,
        session::{ResolvedSendMode, SessionSnapshot},
    };
    use std::{time::{Duration, SystemTime}};

    fn snapshot() -> SessionSnapshot {
        SessionSnapshot {
            display_name: "Vector <unsafe>.png".to_string(),
            download_name: "Vector <unsafe>.png".to_string(),
            content_kind: "file",
            transport_label: "HTTPS tunnel via cloudflared".to_string(),
            input_size: 27_484,
            content_length: Some(27_484),
            expires_at: SystemTime::now() + Duration::from_secs(600),
            remaining: Duration::from_secs(600),
            once: false,
            requires_pin: true,
            revealed_pin: None,
            primary_link_label: "Public HTTPS",
            primary_link: "https://beam.example/s/abc".to_string(),
            secondary_link_label: None,
            secondary_link: None,
            provider_status: "ready".to_string(),
            completed_downloads: 0,
            bytes_served: 0,
            active_download: false,
            consumed: false,
            warnings: Vec::new(),
            mode: ResolvedSendMode::Global {
                provider: ProviderKind::Cloudflared,
            },
            token: "abc".to_string(),
        }
    }

    #[test]
    fn renders_escaped_values_and_pin_notice() {
        let html = render(&snapshot());
        assert!(html.contains("Vector &lt;unsafe&gt;.png"));
        assert!(html.contains("token URL + PIN"));
        assert!(html.contains("A PIN is required before the file can be downloaded."));
        assert!(html.contains("name=\"pin\""));
    }

    #[test]
    fn renders_secondary_local_link_when_present() {
        let mut snapshot = snapshot();
        snapshot.mode = ResolvedSendMode::Local {
            http_port: 8080,
            https_port: 8081,
        };
        snapshot.primary_link_label = "Primary (No Warnings)";
        snapshot.primary_link = "http://192.168.1.2:8080/s/abc".to_string();
        snapshot.secondary_link_label = Some("Secondary (Encrypted)");
        snapshot.secondary_link = Some("https://192.168.1.2:8081/s/abc".to_string());

        let html = render(&snapshot);
        assert!(html.contains("Primary (No Warnings)"));
        assert!(html.contains("Secondary (Encrypted)"));
        assert!(html.contains("Local mode uses HTTP for convenience."));
    }

    #[test]
    fn renders_direct_quick_download_when_pin_is_not_required() {
        let mut snapshot = snapshot();
        snapshot.requires_pin = false;

        let html = render(&snapshot);
        assert!(html.contains("href=\"/download/abc\""));
        assert!(html.contains("href=\"https://github.com/lopezlean/beam\""));
    }
}
