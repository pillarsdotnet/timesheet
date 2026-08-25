// Copyright (c) 2025 Robert August Vincent II <pillarsdotnet@gmail.com>
// Co-author: Claude-AI.

//! Sends the filled timesheet as an email attachment.

use crate::settings::Settings;
use lettre::message::{header::ContentType, Attachment, Mailbox, MultiPart, SinglePart};
use lettre::Message;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// Parses one address, naming the setting it came from so a typo is easy to place.
fn mailbox(address: &str, setting: &str) -> Result<Mailbox, String> {
    address.trim().parse::<Mailbox>().map_err(|e| {
        format!(
            "ts: {}: cannot parse address \"{}\": {}",
            setting, address, e
        )
    })
}

/// Pipes an already-formatted message to a sendmail(8)-compatible binary.
///
/// The envelope is spelled out on the command line — `-f` for the sender, the recipients as
/// operands — rather than left to `-t`, so that what is delivered matches what the config
/// says even if a binary parses headers loosely. `-i` keeps a line holding only a dot from
/// ending the message early.
fn send_via_sendmail(
    program: &Path,
    from: &str,
    recipients: &[String],
    message: &[u8],
    warnings: &mut Vec<String>,
) -> Result<(), String> {
    let mut child = Command::new(program)
        .arg("-i")
        .arg("-f")
        .arg(from)
        .args(recipients)
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("ts: cannot run {}: {}", program.display(), e))?;
    // Dropping the handle closes the pipe, which is what tells the binary the message ended.
    child
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(message)
        .map_err(|e| format!("ts: cannot write to {}: {}", program.display(), e))?;
    let output = child
        .wait_with_output()
        .map_err(|e| format!("ts: {} did not finish: {}", program.display(), e))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let detail = if detail.is_empty() {
            format!("exit status {}", output.status)
        } else {
            detail
        };
        return Err(format!(
            "ts: {} refused the message: {}",
            program.display(),
            detail
        ));
    }
    // A binary that accepted the message may still have had something to say — a dry-run
    // notice, a rewritten sender, a queue warning. Losing that would make a send that did
    // not do what the config asked look like a clean one.
    for line in String::from_utf8_lossy(&output.stderr).lines() {
        if !line.trim().is_empty() {
            warnings.push(line.trim_end().to_string());
        }
    }
    Ok(())
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

    let mut recipients = settings.to.clone();
    recipients.extend(settings.cc.iter().cloned());
    send_via_sendmail(
        &settings.sendmail,
        from,
        &recipients,
        &message.formatted(),
        warnings,
    )?;
    Ok(recipients)
}
