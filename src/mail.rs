// Copyright (c) 2025 Robert August Vincent II <pillarsdotnet@gmail.com>
// Co-author: Claude-AI.

//! Sends the filled timesheet as an email attachment.

use crate::settings::Settings;
use lettre::message::{header::ContentType, Attachment, Mailbox, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{Message, SmtpTransport, Transport};
use std::process::Command;

/// Parses one address, naming the setting it came from so a typo is easy to place.
fn mailbox(address: &str, setting: &str) -> Result<Mailbox, String> {
    address.trim().parse::<Mailbox>().map_err(|e| {
        format!(
            "ts: {}: cannot parse address \"{}\": {}",
            setting, address, e
        )
    })
}

/// Runs `smtp_password_command` and returns what it printed.
///
/// This happens before connecting: a locked keyring is a configuration problem rather than a
/// mail problem, and a pinentry prompt wants the terminal to itself.
fn read_password(command: &str) -> Result<String, String> {
    let output = if cfg!(windows) {
        Command::new("cmd").arg("/C").arg(command).output()
    } else {
        Command::new("sh").arg("-c").arg(command).output()
    }
    .map_err(|e| format!("ts: smtp_password_command failed to run: {}", e))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let detail = if detail.is_empty() {
            format!("exit status {}", output.status)
        } else {
            detail
        };
        return Err(format!("ts: smtp_password_command failed: {}", detail));
    }
    let password = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if password.is_empty() {
        return Err(format!(
            "ts: smtp_password_command printed nothing: {}",
            command
        ));
    }
    Ok(password)
}

/// Mails `data` as `filename` attached to a message built from `settings`.
///
/// Warnings go to `warnings` rather than to stderr directly, so the caller can present them
/// alongside the rest of the run's output.
pub fn send(
    settings: &Settings,
    data: Vec<u8>,
    filename: &str,
    subject: &str,
    body: &str,
    warnings: &mut Vec<String>,
) -> Result<Vec<String>, String> {
    let from = settings.from.as_deref().unwrap_or_default().trim();
    if from.is_empty() {
        return Err("ts: no sender address; use --from or set \"from:\" in the config file".into());
    }
    if settings.to.is_empty() {
        return Err("ts: no recipient address; use --to or set \"to:\" in the config file".into());
    }

    let mut builder = Message::builder()
        .from(mailbox(from, "from")?)
        .subject(subject.to_string());
    for address in &settings.to {
        builder = builder.to(mailbox(address, "to")?);
    }
    for address in &settings.cc {
        builder = builder.cc(mailbox(address, "cc")?);
    }
    // A relay that rewrites From leaves Reply-To alone, so this survives when `from` does
    // not: replies still reach the address actually read.
    if let Some(reply) = settings
        .reply
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        builder = builder.reply_to(mailbox(reply, "reply")?);
    }

    let attachment = Attachment::new(filename.to_string()).body(
        data,
        ContentType::parse("application/pdf").map_err(|e| format!("ts: {}", e))?,
    );
    let message = builder
        .multipart(
            MultiPart::mixed()
                .singlepart(SinglePart::plain(body.to_string()))
                .singlepart(attachment),
        )
        .map_err(|e| format!("ts: cannot build the message: {}", e))?;

    let user = settings
        .smtp_user
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    // Gmail silently rewrites From to the authenticated account unless `from` is a verified
    // "Send mail as" alias there. Setting `reply` is the way to live with that, so a config
    // that sets it has acknowledged the mismatch and gets no warning.
    if let Some(user) = user {
        if !user.eq_ignore_ascii_case(from) && settings.reply.is_none() {
            warnings.push(format!(
                "authenticating as {} but sending as {}; some relays rewrite the From header \
                 unless the sender is a verified alias — set \"reply:\" so replies still reach \
                 you",
                user, from
            ));
        }
    }

    let mut transport = if settings.smtp_starttls {
        SmtpTransport::starttls_relay(&settings.smtp_host).map_err(|e| {
            format!(
                "ts: cannot set up STARTTLS for {}: {}",
                settings.smtp_host, e
            )
        })?
    } else {
        SmtpTransport::builder_dangerous(&settings.smtp_host)
    }
    .port(settings.smtp_port);

    if let Some(user) = user {
        let command = settings
            .smtp_password_command
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or("ts: smtp_user is set without smtp_password_command")?;
        transport =
            transport.credentials(Credentials::new(user.to_string(), read_password(command)?));
    }

    transport.build().send(&message).map_err(|e| {
        format!(
            "ts: {}:{} refused the message: {}",
            settings.smtp_host, settings.smtp_port, e
        )
    })?;

    let mut recipients = settings.to.clone();
    recipients.extend(settings.cc.iter().cloned());
    Ok(recipients)
}
