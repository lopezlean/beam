use crate::{
    duration::format_duration,
    session::{ResolvedSendMode, SharedSession},
};
use anyhow::Result;
use indicatif::HumanBytes;
use qrcode::{QrCode, render::unicode};
use std::{
    io::{IsTerminal, Write, stdout},
    sync::Arc,
    time::Duration,
};

const LIVE_BLOCK_LINES: usize = 6;

pub async fn render_loop(session: Arc<SharedSession>) -> Result<()> {
    let interactive = stdout().is_terminal();
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    let shutdown = session.shutdown_token();

    if !interactive {
        let snapshot = session.snapshot().await;
        print_snapshot_noninteractive(&snapshot)?;
        shutdown.cancelled().await;
        let snapshot = session.snapshot().await;
        print_final_snapshot(&snapshot, false)?;
        return Ok(());
    }

    let snapshot = session.snapshot().await;
    print_static_snapshot(&snapshot, true)?;
    print_live_snapshot(&snapshot, false)?;

    loop {
        tokio::select! {
            _ = interval.tick() => {}
            _ = shutdown.cancelled() => break,
        }

        let snapshot = session.snapshot().await;
        print_live_snapshot(&snapshot, true)?;
    }

    let snapshot = session.snapshot().await;
    print_final_snapshot(&snapshot, false)?;
    Ok(())
}

fn print_snapshot_noninteractive(snapshot: &crate::session::SessionSnapshot) -> Result<()> {
    let mut out = stdout();
    render_static(&mut out, snapshot, false)?;
    write!(out, "{}", live_block(snapshot))?;
    out.flush()?;
    Ok(())
}

fn print_final_snapshot(
    snapshot: &crate::session::SessionSnapshot,
    clear_screen: bool,
) -> Result<()> {
    let mut out = stdout();
    if clear_screen {
        write!(out, "\x1b[2J\x1b[H")?;
    } else {
        write!(out, "\x1b[{LIVE_BLOCK_LINES}F\x1b[J")?;
    }

    writeln!(out, "Beam ⚡️")?;
    writeln!(out, "Session destroyed")?;
    writeln!(
        out,
        "Reason    : {}",
        if snapshot.consumed {
            "Downloaded successfully in burn-after-reading mode"
        } else if snapshot.remaining.is_zero() {
            "TTL expired"
        } else {
            "Stopped"
        }
    )?;
    writeln!(out, "Served    : {}", HumanBytes(snapshot.bytes_served))?;
    writeln!(out, "Downloads : {}", snapshot.completed_downloads)?;
    out.flush()?;
    Ok(())
}

fn print_static_snapshot(
    snapshot: &crate::session::SessionSnapshot,
    clear_screen: bool,
) -> Result<()> {
    let mut out = stdout();
    render_static(&mut out, snapshot, clear_screen)?;
    out.flush()?;
    Ok(())
}

fn print_live_snapshot(
    snapshot: &crate::session::SessionSnapshot,
    redraw_in_place: bool,
) -> Result<()> {
    let mut out = stdout();
    if redraw_in_place {
        write!(out, "\x1b[{LIVE_BLOCK_LINES}F\x1b[J")?;
    }

    write!(out, "{}", live_block(snapshot))?;
    out.flush()?;
    Ok(())
}

fn render_static(
    out: &mut impl Write,
    snapshot: &crate::session::SessionSnapshot,
    clear_screen: bool,
) -> Result<()> {
    if clear_screen {
        write!(out, "\x1b[2J\x1b[H")?;
    }

    writeln!(out, "Beam ⚡️")?;
    writeln!(
        out,
        "{}",
        match snapshot.mode {
            ResolvedSendMode::Local { .. } => "Local network",
            ResolvedSendMode::Global { .. } => "Global default",
        },
    )?;
    writeln!(out)?;
    writeln!(
        out,
        "Payload   : {} ({})",
        snapshot.display_name, snapshot.content_kind
    )?;
    writeln!(out, "Download  : {}", snapshot.download_name)?;
    writeln!(out, "Size      : {}", HumanBytes(snapshot.input_size))?;
    writeln!(out, "Transport : {}", snapshot.transport_label)?;
    writeln!(
        out,
        "Security  : {}{}",
        if snapshot.once { "--once " } else { "" },
        if snapshot.requires_pin {
            "PIN required"
        } else {
            "token URL"
        }
    )?;

    if let Some(pin) = &snapshot.revealed_pin {
        writeln!(out, "PIN       : {pin}")?;
    }

    if matches!(snapshot.mode, ResolvedSendMode::Local { .. }) {
        writeln!(
            out,
            "{} : {}",
            snapshot.primary_link_label, snapshot.primary_link
        )?;
        if let (Some(label), Some(link)) =
            (&snapshot.secondary_link_label, &snapshot.secondary_link)
        {
            writeln!(out, "{} : {}", label, link)?;
        }
        writeln!(
            out,
            "Note      : The encrypted LAN link uses a temporary self-signed certificate."
        )?;
        writeln!(
            out,
            "Warning   : Local mode uses HTTP for convenience. Use --global or --pin for sensitive files."
        )?;
    } else if !snapshot.primary_link.is_empty() {
        writeln!(
            out,
            "{} : {}",
            snapshot.primary_link_label, snapshot.primary_link
        )?;
    }

    if !snapshot.warnings.is_empty() {
        writeln!(out, "Warnings  : {}", snapshot.warnings.join(" · "))?;
    }

    if !snapshot.primary_link.is_empty() {
        writeln!(out)?;
        writeln!(out, "{}", render_qr(&snapshot.primary_link)?)?;
    } else {
        writeln!(out)?;
    }

    Ok(())
}

fn live_block(snapshot: &crate::session::SessionSnapshot) -> String {
    format!(
        concat!(
            "\n",
            "Status    : {status}\n",
            "TTL       : {ttl}\n",
            "Downloads : {downloads}\n",
            "Served    : {served}\n",
            "State     : {state}\n",
        ),
        status = snapshot.provider_status,
        ttl = format_duration(snapshot.remaining),
        downloads = snapshot.completed_downloads,
        served = HumanBytes(snapshot.bytes_served),
        state = transfer_state(snapshot),
    )
}

fn transfer_state(snapshot: &crate::session::SessionSnapshot) -> &'static str {
    if snapshot.consumed {
        "consumed"
    } else if snapshot.active_download {
        "downloading"
    } else {
        "waiting"
    }
}

fn render_qr(link: &str) -> Result<String> {
    let code = QrCode::new(link.as_bytes())?;
    Ok(code
        .render::<unicode::Dense1x2>()
        .dark_color(unicode::Dense1x2::Dark)
        .light_color(unicode::Dense1x2::Light)
        .build())
}
