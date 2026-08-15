use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use reqwest::header::{ACCEPT, HeaderMap, HeaderValue, USER_AGENT};
use serde_json::{Value, json};
use uuid::Uuid;
use zeroclaw_api::channel::WorkerProgressDisposition;

const A2A_REPLY_ATTEMPTS: usize = 3;

#[derive(Clone)]
pub(super) struct A2aDelivery {
    http: reqwest::Client,
    base_url: String,
    identity: String,
    progress_interval_secs: u64,
    task_starts: Arc<Mutex<HashMap<String, Instant>>>,
}

impl A2aDelivery {
    pub(super) fn new(
        client: &inkbox::Inkbox,
        identity: String,
        progress_interval_secs: u64,
    ) -> Result<Self> {
        let mut headers = HeaderMap::new();
        headers.insert(
            "X-API-Key",
            HeaderValue::from_str(client.api_key()).context("invalid Inkbox API key bytes")?,
        );
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert(
            USER_AGENT,
            HeaderValue::from_static("zeroclaw-inkbox-channel"),
        );
        let http = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(Duration::from_secs(30))
            .build()
            .context("build Inkbox A2A client")?;
        Ok(Self {
            http,
            base_url: client.base_url().trim_end_matches('/').to_string(),
            identity,
            progress_interval_secs,
            task_starts: Arc::default(),
        })
    }

    pub(super) fn progress_interval_secs(&self) -> u64 {
        self.progress_interval_secs
    }

    pub(super) fn acknowledgement(&self, task_id: &str) -> String {
        if self.progress_interval_secs == 0 {
            return format!("Task {task_id} received. Work is queued and starting.");
        }
        let cadence = cadence_text(self.progress_interval_secs);
        format!(
            "Task {task_id} received. Work is queued and starting. Expect progress updates about every {cadence}."
        )
    }

    pub(super) fn elapsed_secs(&self, task_id: &str) -> u64 {
        let mut starts = self.task_starts.lock().unwrap_or_else(|e| e.into_inner());
        starts
            .entry(task_id.to_string())
            .or_insert_with(Instant::now)
            .elapsed()
            .as_secs()
    }

    pub(super) fn stop(&self, task_id: &str) {
        self.task_starts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(task_id);
    }

    fn task_url(&self, task_id: &str) -> String {
        format!(
            "{}/api/v1/identities/{}/a2a/tasks/{task_id}",
            self.base_url, self.identity
        )
    }

    async fn task(&self, task_id: &str) -> Result<Value> {
        self.http
            .get(self.task_url(task_id))
            .send()
            .await
            .context("get inbound A2A task")?
            .error_for_status()
            .context("get inbound A2A task")?
            .json()
            .await
            .context("decode inbound A2A task")
    }

    pub(super) async fn reply(
        &self,
        task_id: &str,
        intent: &str,
        text: &str,
    ) -> Result<WorkerProgressDisposition> {
        Uuid::parse_str(task_id).context("invalid inbound A2A task id")?;
        let mut last_error = None;
        for attempt in 0..A2A_REPLY_ATTEMPTS {
            if let Ok(task) = self.task(task_id).await {
                if intent == "progress" && task_is_terminal(&task) {
                    self.stop(task_id);
                    return Ok(WorkerProgressDisposition::AlreadyTerminal);
                }
                if task_contains_text(&task, text) {
                    if matches!(intent, "complete" | "fail") {
                        self.stop(task_id);
                    }
                    return Ok(WorkerProgressDisposition::Sent);
                }
            }

            let result = self
                .http
                .post(format!("{}/reply", self.task_url(task_id)))
                .json(&json!({"intent": intent, "parts": [{"text": text}]}))
                .send()
                .await
                .context("send A2A task reply")
                .and_then(|response| {
                    response
                        .error_for_status()
                        .map(|_| ())
                        .context("send A2A task reply")
                });
            match result {
                Ok(()) => {
                    if matches!(intent, "complete" | "fail") {
                        self.stop(task_id);
                    }
                    return Ok(WorkerProgressDisposition::Sent);
                }
                Err(error) => last_error = Some(error),
            }
            if attempt + 1 < A2A_REPLY_ATTEMPTS {
                tokio::time::sleep(Duration::from_millis(250 * (attempt as u64 + 1))).await;
            }
        }
        Err(last_error.unwrap_or_else(|| anyhow::Error::msg("send A2A task reply")))
    }
}

pub(super) fn cadence_text(seconds: u64) -> String {
    if seconds > 0 && seconds.is_multiple_of(60) {
        let minutes = seconds / 60;
        let unit = if minutes == 1 { "minute" } else { "minutes" };
        format!("{minutes} {unit}")
    } else {
        let unit = if seconds == 1 { "second" } else { "seconds" };
        format!("{seconds} {unit}")
    }
}

fn task_is_terminal(task: &Value) -> bool {
    matches!(
        task.get("state").and_then(Value::as_str),
        Some("completed" | "failed" | "canceled" | "rejected")
    )
}

fn task_contains_text(task: &Value, expected: &str) -> bool {
    task.get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|message| message.get("parts").and_then(Value::as_array))
        .flatten()
        .any(|part| part.get("text").and_then(Value::as_str) == Some(expected))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn test_delivery(server: &MockServer) -> A2aDelivery {
        A2aDelivery {
            http: reqwest::Client::new(),
            base_url: server.uri(),
            identity: "worker".to_string(),
            progress_interval_secs: 180,
            task_starts: Arc::default(),
        }
    }

    #[test]
    fn default_acknowledgement_names_three_minute_cadence() {
        assert_eq!(cadence_text(180), "3 minutes",);
        assert_eq!(cadence_text(60), "1 minute");
        assert_eq!(cadence_text(1), "1 second");
    }

    #[test]
    fn history_match_is_exact_and_terminal_state_is_recognized() {
        let task = json!({
            "state": "completed",
            "messages": [{"parts": [{"text": "still working"}]}]
        });
        assert!(task_contains_text(&task, "still working"));
        assert!(!task_contains_text(&task, "working"));
        assert!(task_is_terminal(&task));
    }

    #[test]
    fn task_elapsed_time_survives_follow_up_turns_until_stopped() {
        let task_id = "11111111-1111-1111-1111-111111111111";
        let delivery = A2aDelivery {
            http: reqwest::Client::new(),
            base_url: "https://example.invalid".to_string(),
            identity: "worker".to_string(),
            progress_interval_secs: 180,
            task_starts: Arc::new(Mutex::new(HashMap::from([(
                task_id.to_string(),
                Instant::now() - Duration::from_secs(12),
            )]))),
        };

        assert_eq!(
            delivery.acknowledgement(task_id),
            format!(
                "Task {task_id} received. Work is queued and starting. Expect progress updates about every 3 minutes."
            )
        );
        assert!(delivery.elapsed_secs(task_id) >= 12);
        assert!(delivery.elapsed_secs(task_id) >= 12);
        delivery.stop(task_id);
        assert_eq!(delivery.elapsed_secs(task_id), 0);
    }

    #[tokio::test]
    async fn accepted_reply_is_found_before_retrying_and_not_duplicated() {
        let task_id = "11111111-1111-1111-1111-111111111111";
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/api/v1/identities/worker/a2a/tasks/{task_id}"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "state": "working",
                "messages": [{"parts": [{"text": "Still working."}]}]
            })))
            .mount(&server)
            .await;

        test_delivery(&server)
            .reply(task_id, "progress", "Still working.")
            .await
            .unwrap();

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method.as_str(), "GET");
    }

    #[tokio::test]
    async fn progress_reports_terminal_disposition_without_posting() {
        let task_id = "11111111-1111-1111-1111-111111111111";
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/api/v1/identities/worker/a2a/tasks/{task_id}"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "state": "canceled",
                "messages": [{"parts": [{"text": "Task received."}]}]
            })))
            .mount(&server)
            .await;

        let disposition = test_delivery(&server)
            .reply(task_id, "progress", "Task received.")
            .await
            .unwrap();
        assert_eq!(disposition, WorkerProgressDisposition::AlreadyTerminal);
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method.as_str(), "GET");
    }

    #[tokio::test]
    async fn progress_reply_uses_nonterminal_intent() {
        let task_id = "11111111-1111-1111-1111-111111111111";
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "state": "working",
                "messages": []
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!(
                "/api/v1/identities/worker/a2a/tasks/{task_id}/reply"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .mount(&server)
            .await;

        test_delivery(&server)
            .reply(task_id, "progress", "Still working.")
            .await
            .unwrap();

        let requests = server.received_requests().await.unwrap();
        let post = requests
            .iter()
            .find(|request| request.method.as_str() == "POST")
            .expect("progress POST");
        let body: Value = serde_json::from_slice(&post.body).unwrap();
        assert_eq!(body["intent"], "progress");
        assert_eq!(body["parts"][0]["text"], "Still working.");
    }
}
