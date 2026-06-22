//! Interactive Inkbox onboarding for the CLI Quickstart "Channels" step.
//!
//! Inkbox is a native channel in this fork, so picking it branches out of the
//! generic schema field-form into a live wizard: self-signup (or paste a key),
//! verification, phone provisioning, a webhook signing key, and OpenAI Realtime
//! call config. The SDK round-trips live in
//! [`zeroclaw_runtime::inkbox_onboarding`]; this module owns the prompts and
//! returns `(alias, fields)` for the caller to fold into the submission.
//!
//! All user-facing text routes through `crate::t` (a `cli-*` Fluent key with an
//! English fallback), and the blocking dialoguer prompts mirror the rest of
//! `run_quickstart_cli`.

use std::collections::BTreeMap;

use zeroclaw_runtime::inkbox_onboarding as ob;

const DEFAULT_BASE_URL: &str = "https://inkbox.ai";

/// Run the Inkbox channel wizard.
///
/// # Returns
/// `Some((alias, fields))` to materialize a `[channels.inkbox.<alias>]` block,
/// or `None` if the user backed out (the caller then re-renders the channel
/// list, nothing written).
pub(crate) fn run() -> anyhow::Result<Option<(String, BTreeMap<String, String>)>> {
    let base_url = std::env::var("INKBOX_BASE_URL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());

    println!(
        "  {}",
        crate::t(
            "cli-quickstart-inkbox-intro",
            "API-first email + SMS + voice + identity for one agent.",
        )
    );

    let Some(has_key) = confirm(
        &crate::t(
            "cli-quickstart-inkbox-has-key",
            "Do you already have an Inkbox API key?",
        ),
        false,
    )?
    else {
        return Ok(None);
    };

    // Resolve an (api_key, identity handle) pair, by signup or by pasted key.
    let Some((api_key, handle)) = (if has_key {
        api_key_flow(&base_url)?
    } else {
        signup_flow(&base_url)?
    }) else {
        return Ok(None);
    };

    let mut fields: BTreeMap<String, String> = BTreeMap::new();
    fields.insert("api_key".into(), api_key.clone());
    fields.insert("identity".into(), handle.clone());
    if base_url != DEFAULT_BASE_URL {
        fields.insert("base_url".into(), base_url.clone());
    }

    // Phone: server-side only (no config field) — unlocks SMS + voice.
    if let Some(true) = confirm(
        &crate::t(
            "cli-quickstart-inkbox-provision-phone",
            "Provision a local phone number? (unlocks SMS + voice)",
        ),
        true,
    )? {
        match ob::provision_phone(&base_url, &api_key, &handle) {
            Ok(number) => println!(
                "  {} {}",
                crate::t("cli-quickstart-inkbox-provisioned", "✓ provisioned"),
                number
            ),
            Err(err) => eprintln!(
                "  {} {}",
                crate::t(
                    "cli-quickstart-inkbox-provision-failed",
                    "could not provision a number:",
                ),
                err
            ),
        }
    }

    setup_signing_key(&base_url, &api_key, &mut fields)?;
    setup_realtime(&mut fields)?;

    let alias = match prompt_alias(&handle)? {
        Some(a) => a,
        None => return Ok(None),
    };

    let channel_ref = format!("inkbox.{alias}");
    println!(
        "  {} {}",
        crate::t(
            "cli-quickstart-inkbox-configured",
            "✓ Inkbox channel configured:",
        ),
        channel_ref
    );
    Ok(Some((alias, fields)))
}

/// Self-signup branch: create a fresh identity and verify it.
fn signup_flow(base_url: &str) -> anyhow::Result<Option<(String, String)>> {
    println!(
        "  {}",
        crate::t(
            "cli-quickstart-inkbox-signup-intro",
            "We'll create a fresh agent identity for you via self-signup.",
        )
    );

    let Some(email) = input(
        &crate::t(
            "cli-quickstart-inkbox-email",
            "Your email address (for verification)",
        ),
        None,
        false,
    )?
    else {
        return Ok(None);
    };
    let email = email.trim().to_string();
    if !email.contains('@') {
        eprintln!(
            "  {}",
            crate::t(
                "cli-quickstart-inkbox-bad-email",
                "A valid email address is required.",
            )
        );
        return Ok(None);
    }

    let Some(handle) = input(
        &crate::t(
            "cli-quickstart-inkbox-handle",
            "Desired agent handle (globally unique)",
        ),
        None,
        false,
    )?
    else {
        return Ok(None);
    };
    let handle = handle.trim().to_string();
    if handle.is_empty() {
        return Ok(None);
    }

    println!(
        "  {}",
        crate::t("cli-quickstart-inkbox-signing-up", "Calling agent-signup…")
    );
    let signup = match ob::signup(base_url, &email, &handle) {
        Ok(s) => s,
        Err(err) => {
            eprintln!(
                "  {} {}",
                crate::t("cli-quickstart-inkbox-signup-failed", "signup failed:"),
                err
            );
            return Ok(None);
        }
    };
    println!(
        "  {} {}",
        crate::t("cli-quickstart-inkbox-created", "✓ created"),
        signup.agent_handle
    );
    println!(
        "  {} {}",
        crate::t("cli-quickstart-inkbox-mailbox", "mailbox:"),
        signup.email_address
    );
    println!(
        "  {} {}",
        crate::t(
            "cli-quickstart-inkbox-code-sent",
            "A 6-digit code was sent to",
        ),
        email
    );

    loop {
        let Some(entry) = input(
            &crate::t(
                "cli-quickstart-inkbox-code",
                "Verification code (or 'resend')",
            ),
            None,
            true,
        )?
        else {
            return Ok(None);
        };
        let entry = entry.trim().to_string();
        if entry.is_empty() {
            continue;
        }
        if entry.eq_ignore_ascii_case("resend") || entry.eq_ignore_ascii_case("r") {
            match ob::resend(base_url, &signup.api_key) {
                Ok(()) => println!(
                    "  {} {}",
                    crate::t("cli-quickstart-inkbox-resent", "✓ Resent. Check"),
                    email
                ),
                Err(err) => eprintln!(
                    "  {} {}",
                    crate::t("cli-quickstart-inkbox-resend-failed", "resend failed:"),
                    err
                ),
            }
            continue;
        }
        match ob::verify(base_url, &signup.api_key, &entry) {
            Ok(()) => {
                println!(
                    "  {}",
                    crate::t("cli-quickstart-inkbox-verified", "✓ verified")
                );
                break;
            }
            Err(err) => eprintln!(
                "  {} {}",
                crate::t("cli-quickstart-inkbox-bad-code", "wrong code:"),
                err
            ),
        }
    }
    Ok(Some((signup.api_key, signup.agent_handle)))
}

/// Paste-a-key branch: validate the key and confirm its bound identity.
fn api_key_flow(base_url: &str) -> anyhow::Result<Option<(String, String)>> {
    let Some(api_key) = password(&crate::t(
        "cli-quickstart-inkbox-paste-key",
        "Paste your Inkbox API key (ApiKey_…)",
    ))?
    else {
        return Ok(None);
    };
    let api_key = api_key.trim().to_string();
    if api_key.is_empty() {
        eprintln!(
            "  {}",
            crate::t("cli-quickstart-inkbox-no-key", "No key provided.")
        );
        return Ok(None);
    }

    match ob::key_scope(base_url, &api_key) {
        Ok(ob::KeyScope::NotApiKey) => {
            eprintln!(
                "  {}",
                crate::t(
                    "cli-quickstart-inkbox-not-api-key",
                    "This credential is not an API key (JWTs are not supported here).",
                )
            );
            return Ok(None);
        }
        Ok(_) => {}
        Err(err) => {
            eprintln!(
                "  {} {}",
                crate::t(
                    "cli-quickstart-inkbox-whoami-failed",
                    "could not validate the key:",
                ),
                err
            );
            return Ok(None);
        }
    }

    let Some(handle) = input(
        &crate::t(
            "cli-quickstart-inkbox-which-handle",
            "Agent identity handle this gateway runs as",
        ),
        None,
        false,
    )?
    else {
        return Ok(None);
    };
    let handle = handle.trim().to_string();
    if handle.is_empty() {
        return Ok(None);
    }

    match ob::fetch_identity(base_url, &api_key, &handle) {
        Ok(id) => {
            println!(
                "  {} {}",
                crate::t("cli-quickstart-inkbox-key-bound", "✓ key validated for"),
                id.handle
            );
            if let Some(phone) = id.phone_number {
                println!(
                    "  {} {}",
                    crate::t("cli-quickstart-inkbox-phone", "phone:"),
                    phone
                );
            }
            Ok(Some((api_key, id.handle)))
        }
        Err(err) => {
            eprintln!(
                "  {} {}",
                crate::t(
                    "cli-quickstart-inkbox-handle-failed",
                    "could not load that identity:",
                ),
                err
            );
            Ok(None)
        }
    }
}

/// Webhook signing key: paste an existing one or mint a fresh key.
fn setup_signing_key(
    base_url: &str,
    api_key: &str,
    fields: &mut BTreeMap<String, String>,
) -> anyhow::Result<()> {
    if let Some(true) = confirm(
        &crate::t(
            "cli-quickstart-inkbox-have-signing",
            "Do you already have an Inkbox signing key?",
        ),
        false,
    )? {
        if let Some(key) = password(&crate::t(
            "cli-quickstart-inkbox-paste-signing",
            "Paste your Inkbox signing key (whsec_…)",
        ))? {
            let key = key.trim().to_string();
            if !key.is_empty() {
                fields.insert("signing_key".into(), key);
            }
        }
        return Ok(());
    }

    if let Some(true) = confirm(
        &crate::t(
            "cli-quickstart-inkbox-gen-signing",
            "Generate a new Inkbox signing key now?",
        ),
        true,
    )? {
        match ob::create_signing_key(base_url, api_key) {
            Ok(key) => {
                fields.insert("signing_key".into(), key);
                println!(
                    "  {}",
                    crate::t(
                        "cli-quickstart-inkbox-signing-saved",
                        "✓ signing key saved — signature verification on",
                    )
                );
            }
            Err(err) => eprintln!(
                "  {} {}",
                crate::t(
                    "cli-quickstart-inkbox-signing-failed",
                    "could not create signing key:",
                ),
                err
            ),
        }
    }
    Ok(())
}

/// OpenAI Realtime call config. Detects a key from the environment; the live
/// websocket probe is deferred — the key is validated on the first call.
fn setup_realtime(fields: &mut BTreeMap<String, String>) -> anyhow::Result<()> {
    let detected = std::env::var("INKBOX_REALTIME_API_KEY")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::env::var("OPENAI_API_KEY")
                .ok()
                .filter(|s| !s.is_empty())
        });

    if detected.is_some() {
        println!(
            "  {}",
            crate::t(
                "cli-quickstart-inkbox-rt-found",
                "Found an OpenAI API key in your environment.",
            )
        );
    } else {
        println!(
            "  {}",
            crate::t(
                "cli-quickstart-inkbox-rt-none",
                "Realtime calls need an OpenAI API key with /v1/realtime access.",
            )
        );
    }

    let Some(true) = confirm(
        &crate::t(
            "cli-quickstart-inkbox-rt-enable",
            "Use OpenAI Realtime for phone calls?",
        ),
        detected.is_some(),
    )?
    else {
        return Ok(());
    };

    let key = match detected {
        Some(k) => k,
        None => match password(&crate::t(
            "cli-quickstart-inkbox-rt-paste",
            "Paste your OpenAI API key for Realtime",
        ))? {
            Some(k) if !k.trim().is_empty() => k.trim().to_string(),
            _ => {
                println!(
                    "  {}",
                    crate::t(
                        "cli-quickstart-inkbox-rt-skip",
                        "No key entered; Realtime left off (calls use Inkbox STT/TTS).",
                    )
                );
                return Ok(());
            }
        },
    };
    fields.insert("realtime_enabled".into(), "true".into());
    fields.insert("realtime_api_key".into(), key);
    println!(
        "  {}",
        crate::t(
            "cli-quickstart-inkbox-rt-on",
            "✓ Realtime enabled (validated on the first call).",
        )
    );
    Ok(())
}

/// Prompt for the channel alias, defaulting to the handle, until it is a valid
/// alias key. Returns `None` if the user backs out.
fn prompt_alias(handle: &str) -> anyhow::Result<Option<String>> {
    loop {
        let Some(raw) = input(
            &crate::t(
                "cli-quickstart-inkbox-alias",
                "Alias for this Inkbox channel",
            ),
            Some(handle),
            false,
        )?
        else {
            return Ok(None);
        };
        let candidate = {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                handle.to_string()
            } else {
                trimmed.to_string()
            }
        };
        match zeroclaw_config::helpers::validate_alias_key(&candidate) {
            Ok(()) => return Ok(Some(candidate)),
            Err(err) => eprintln!(
                "  {} {}",
                crate::t("cli-quickstart-inkbox-bad-alias", "invalid alias:"),
                err
            ),
        }
    }
}

// ── dialoguer helpers (Ctrl+C → `None`, mirroring `prompt_for_field`) ──

fn confirm(prompt: &str, default: bool) -> anyhow::Result<Option<bool>> {
    Ok(dialoguer::Confirm::new()
        .with_prompt(prompt)
        .default(default)
        .interact_opt()?)
}

fn input(prompt: &str, default: Option<&str>, allow_empty: bool) -> anyhow::Result<Option<String>> {
    let mut builder = dialoguer::Input::<String>::new()
        .with_prompt(prompt)
        .allow_empty(allow_empty);
    if let Some(d) = default {
        builder = builder.default(d.to_string());
    }
    match builder.interact_text() {
        Ok(v) => Ok(Some(v)),
        Err(e) => map_interrupt(e),
    }
}

fn password(prompt: &str) -> anyhow::Result<Option<String>> {
    match dialoguer::Password::new()
        .with_prompt(prompt)
        .allow_empty_password(true)
        .interact()
    {
        Ok(v) => Ok(Some(v)),
        Err(e) => map_interrupt(e),
    }
}

/// Map a dialoguer Ctrl+C interrupt to `Ok(None)` ("backed out"); bubble any
/// other IO error.
fn map_interrupt(e: dialoguer::Error) -> anyhow::Result<Option<String>> {
    let io: std::io::Error = e.into();
    if io.kind() == std::io::ErrorKind::Interrupted {
        Ok(None)
    } else {
        Err(io.into())
    }
}
