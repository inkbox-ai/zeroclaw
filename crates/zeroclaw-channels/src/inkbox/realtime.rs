//! OpenAI Realtime bridge for Inkbox calls — a faithful port of the hermes
//! plugin's `realtime.py` (audio core; tools / consult / post-call land in a
//! follow-up increment).
//!
//! When the channel is configured for realtime, the call-media WebSocket is
//! accepted with `x-use-inkbox-speech-to-text: false` /
//! `x-use-inkbox-text-to-speech: false`, so Inkbox sends **raw g711 (PCMU)
//! audio** as `media` frames instead of doing its own STT/TTS. This bridge
//! pumps that audio to the OpenAI Realtime API and pumps the model's audio back:
//!
//! * Inkbox → OpenAI: `{"event":"media","media":{"payload":<b64 pcmu>}}`
//!   becomes `input_audio_buffer.append { audio }`.
//! * OpenAI → Inkbox: `response.output_audio.delta { delta }` becomes
//!   `{"event":"media","media":{"payload":<b64>,"track":"outbound"}}`.
//! * Barge-in: `input_audio_buffer.speech_started` → send `{"event":"clear"}`.
//! * Server VAD (`interrupt_response: true`) drives turn-taking.
//!
//! The running transcript is accumulated from the transcription events for the
//! post-call reflection (wired in the follow-up increment).

use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message as WsMessage;

/// Realtime bridge configuration, resolved from `[channels.inkbox.<alias>]`.
#[derive(Debug, Clone)]
pub struct RealtimeConfig {
    /// OpenAI API key (Bearer).
    pub api_key: String,
    /// Realtime model id (e.g. `gpt-realtime-2`).
    pub model: String,
    /// Voice (e.g. `cedar`).
    pub voice: String,
}

impl RealtimeConfig {
    /// Whether realtime is usable (enabled + a credential present).
    pub fn usable(enabled: bool, api_key: &str) -> bool {
        enabled && !api_key.trim().is_empty()
    }
}

/// Minimal call metadata available at WS-accept time. Purpose/opening context
/// (from the outbound `context_token`) is threaded in a later increment.
#[derive(Debug, Clone, Default)]
pub struct CallMeta {
    pub direction: String,
    pub contact_name: Option<String>,
}

const OPENAI_REALTIME_URL: &str = "wss://api.openai.com/v1/realtime";
const INPUT_TRANSCRIPTION_MODEL: &str = "gpt-4o-mini-transcribe";

/// Build the system instructions for the realtime model from call metadata.
fn build_instructions(meta: &CallMeta) -> String {
    let who = meta.contact_name.as_deref().unwrap_or("the caller");
    format!(
        "You are a helpful voice assistant on a live phone call with {who}. \
         Speak naturally and concisely, one short turn at a time. This is spoken \
         audio, not text — no markdown, no long monologues. Direction: {dir}.",
        dir = if meta.direction.is_empty() { "inbound" } else { &meta.direction }
    )
}

/// The opening instruction (greeting) sent once when the call connects.
fn build_greeting(meta: &CallMeta) -> String {
    if meta.direction == "outbound" {
        "Open the call: greet the person and concisely explain why you're calling. \
         One short sentence, then wait."
            .to_string()
    } else {
        let who = meta
            .contact_name
            .as_deref()
            .map(|n| n.split_whitespace().next().unwrap_or(n).to_string())
            .unwrap_or_else(|| "there".to_string());
        format!("Greet the caller now: say something like \"Hi {who}, how can I help?\" One short sentence, then wait.")
    }
}

/// The `session.update` payload (PCMU audio, server VAD, voice, transcription).
fn session_update(cfg: &RealtimeConfig, meta: &CallMeta) -> Value {
    json!({
        "type": "session.update",
        "session": {
            "type": "realtime",
            "model": cfg.model,
            "instructions": build_instructions(meta),
            "output_modalities": ["audio"],
            "audio": {
                "input": {
                    "format": { "type": "audio/pcmu" },
                    "transcription": { "model": INPUT_TRANSCRIPTION_MODEL },
                    "turn_detection": {
                        "type": "server_vad",
                        "threshold": 0.5,
                        "prefix_padding_ms": 300,
                        "silence_duration_ms": 500,
                        "create_response": true,
                        "interrupt_response": true
                    }
                },
                "output": {
                    "format": { "type": "audio/pcmu" },
                    "voice": cfg.voice
                }
            },
            "tool_choice": "auto"
        }
    })
}

/// Connect to the OpenAI Realtime API over WebSocket (Bearer auth).
async fn connect_openai(
    cfg: &RealtimeConfig,
) -> anyhow::Result<
    tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
> {
    // Ensure a rustls crypto provider is installed (idempotent across the process).
    let _ = rustls::crypto::ring::default_provider().install_default();

    let url = format!(
        "{OPENAI_REALTIME_URL}?model={}",
        urlencoding::encode(&cfg.model)
    );
    let mut request = url
        .into_client_request()
        .map_err(|e| anyhow::anyhow!("realtime: bad URL: {e}"))?;
    request.headers_mut().insert(
        "Authorization",
        format!("Bearer {}", cfg.api_key)
            .parse()
            .map_err(|e| anyhow::anyhow!("realtime: bad auth header: {e}"))?,
    );
    let (ws, _resp) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| anyhow::anyhow!("realtime: OpenAI connect failed: {e}"))?;
    Ok(ws)
}

/// Run the realtime bridge between the (already-upgraded) Inkbox call-media
/// WebSocket and the OpenAI Realtime API. Returns when either side closes.
///
/// # Arguments
/// * `inkbox_ws` - the upgraded axum WebSocket carrying Inkbox `media` frames.
/// * `cfg` - resolved realtime config (key/model/voice).
/// * `meta` - call metadata (direction, contact name).
pub async fn run_realtime_bridge(inkbox_ws: WebSocket, cfg: RealtimeConfig, meta: CallMeta) {
    let openai = match connect_openai(&cfg).await {
        Ok(ws) => ws,
        Err(e) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                format!("[inkbox] realtime bridge connect failed: {e}"),
            );
            return;
        }
    };
    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
        "[inkbox] realtime bridge connected to OpenAI",
    );

    let (mut oai_tx, mut oai_rx) = openai.split();
    let (mut ink_tx, mut ink_rx) = inkbox_ws.split();

    // Configure the session.
    if oai_tx
        .send(WsMessage::Text(session_update(&cfg, &meta).to_string().into()))
        .await
        .is_err()
    {
        return;
    }

    let mut stream_id: Option<String> = None;
    let mut greeting_sent = false;
    let mut transcript: Vec<(String, String)> = Vec::new();

    loop {
        tokio::select! {
            // ── Inkbox → OpenAI ──
            ink = ink_rx.next() => {
                let raw = match ink {
                    Some(Ok(Message::Text(t))) => t,
                    Some(Ok(_)) => continue,
                    _ => break,
                };
                let Ok(frame) = serde_json::from_str::<Value>(&raw) else { continue };
                match frame.get("event").and_then(Value::as_str) {
                    Some("start") => {
                        if let Some(sid) = frame.get("stream_id").and_then(Value::as_str) {
                            stream_id = Some(sid.to_string());
                        }
                        if !greeting_sent {
                            greeting_sent = true;
                            let greet = json!({
                                "type": "response.create",
                                "response": { "instructions": build_greeting(&meta) }
                            });
                            if oai_tx.send(WsMessage::Text(greet.to_string().into())).await.is_err() {
                                break;
                            }
                        }
                    }
                    Some("media") => {
                        if !greeting_sent {
                            // Some calls never send `start`; greet on first audio.
                            greeting_sent = true;
                            let greet = json!({
                                "type": "response.create",
                                "response": { "instructions": build_greeting(&meta) }
                            });
                            let _ = oai_tx.send(WsMessage::Text(greet.to_string().into())).await;
                        }
                        if let Some(payload) = frame.pointer("/media/payload").and_then(Value::as_str) {
                            let append = json!({ "type": "input_audio_buffer.append", "audio": payload });
                            if oai_tx.send(WsMessage::Text(append.to_string().into())).await.is_err() {
                                break;
                            }
                        }
                    }
                    Some("stop") | Some("closed") | Some("hangup") => break,
                    _ => {}
                }
            }
            // ── OpenAI → Inkbox ──
            oai = oai_rx.next() => {
                let raw = match oai {
                    Some(Ok(WsMessage::Text(t))) => t,
                    Some(Ok(WsMessage::Close(_))) | None => break,
                    Some(Ok(_)) => continue,
                    Some(Err(_)) => break,
                };
                let Ok(ev) = serde_json::from_str::<Value>(&raw) else { continue };
                match ev.get("type").and_then(Value::as_str) {
                    // Model audio out → Inkbox media frame.
                    Some("response.output_audio.delta") | Some("response.audio.delta") => {
                        if let Some(delta) = ev.get("delta").and_then(Value::as_str) {
                            let mut media = json!({
                                "event": "media",
                                "media": { "payload": delta, "track": "outbound" }
                            });
                            if let Some(sid) = &stream_id {
                                media["stream_id"] = json!(sid);
                            }
                            if ink_tx.send(Message::Text(media.to_string().into())).await.is_err() {
                                break;
                            }
                        }
                    }
                    Some("response.output_audio.done") | Some("response.audio.done") => {
                        let mut done = json!({ "event": "audio_done" });
                        if let Some(sid) = &stream_id { done["stream_id"] = json!(sid); }
                        let _ = ink_tx.send(Message::Text(done.to_string().into())).await;
                    }
                    // Caller started talking → drop queued outbound audio (barge-in).
                    Some("input_audio_buffer.speech_started") => {
                        let _ = ink_tx.send(Message::Text(json!({ "event": "clear" }).to_string().into())).await;
                    }
                    // Transcript accumulation.
                    Some("response.audio_transcript.done")
                    | Some("response.output_audio_transcript.done") => {
                        if let Some(t) = ev.get("transcript").and_then(Value::as_str) {
                            transcript.push(("agent".into(), t.to_string()));
                        }
                    }
                    Some("conversation.item.input_audio_transcription.completed") => {
                        if let Some(t) = ev.get("transcript").and_then(Value::as_str) {
                            transcript.push(("caller".into(), t.to_string()));
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
        format!("[inkbox] realtime call ended ({} transcript turns)", transcript.len()),
    );
}
