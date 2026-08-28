#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        path::PathBuf,
        sync::mpsc,
        thread,
    };

    use super::*;

    fn test_config(directory: PathBuf) -> AgentConfig {
        let config_path = directory.join("config.json");
        AgentConfig {
            endpoint: "https://unionc.example/api/modules/host-monitoring/agent/v1/report".into(),
            config_path: Some(config_path),
            state_dir: directory,
            ..AgentConfig::default()
        }
    }

    fn test_host() -> HostIdentity {
        HostIdentity {
            id: Uuid::new_v4().to_string(),
            os: "test".into(),
            os_version: None,
            kernel_version: None,
            arch: "test".into(),
            agent_version: "test".into(),
        }
    }

    #[test]
    fn pairing_status_url_appends_path_segments_without_query_or_fragment_ambiguity() {
        let request_id = Uuid::new_v4();
        let endpoint = pairing_status_endpoint(
            "https://unionc.example/api/modules/host-monitoring/agent/v2/pairing-requests/",
            request_id,
        )
        .unwrap();
        assert_eq!(
            endpoint.as_str(),
            format!(
                "https://unionc.example/api/modules/host-monitoring/agent/v2/pairing-requests/{request_id}/status"
            )
        );

        for invalid in [
            "https://unionc.example/api/modules/host-monitoring/agent/v2/pairing-requests?tenant=one",
            "https://unionc.example/api/modules/host-monitoring/agent/v2/pairing-requests#bootstrap",
        ] {
            assert!(pairing_status_endpoint(invalid, request_id).is_err());
        }
    }

    #[test]
    fn persisted_pairing_endpoints_are_revalidated_before_network_use() {
        let request_id = Uuid::new_v4();
        let remote_plaintext =
            "http://192.0.2.10/api/modules/host-monitoring/agent/v2/pairing-requests";
        assert!(pairing_status_endpoint(remote_plaintext, request_id).is_err());
        assert!(activation_endpoint(remote_plaintext).is_err());
    }

    #[tokio::test]
    async fn persisted_creating_state_cannot_reuse_remote_plaintext_endpoint() {
        let state = StoredPairingState::Creating {
            version: PAIRING_STATE_VERSION,
            generation: Uuid::new_v4(),
            pairing_endpoint:
                "http://192.0.2.10/api/modules/host-monitoring/agent/v2/pairing-requests".into(),
            report_endpoint: "http://192.0.2.10/api/modules/host-monitoring/agent/v1/report".into(),
            host: test_host(),
            bearer_secret: random_secret(),
            polling_secret: random_secret(),
        };
        let config = AgentConfig {
            allow_insecure_http: true,
            ..AgentConfig::default()
        };

        let error = finish_create_request(&config, state)
            .await
            .expect_err("old state must be checked under the current pairing transport policy");
        assert!(format!("{error:#}").contains("browser pairing requires HTTPS"));
    }

    #[tokio::test]
    async fn transient_plaintext_override_cannot_resume_durable_pairing_stages() {
        let directory = std::env::temp_dir().join(format!(
            "unionc-pairing-durable-http-policy-{}",
            Uuid::new_v4()
        ));
        fs::create_dir_all(&directory).unwrap();
        let pairing_endpoint =
            "http://127.0.0.1:1/api/modules/host-monitoring/agent/v2/pairing-requests".to_string();
        let report_endpoint =
            "http://192.0.2.10:1/api/modules/host-monitoring/agent/v1/report".to_string();
        let config = AgentConfig {
            endpoint: report_endpoint.clone(),
            pairing_endpoint: Some(pairing_endpoint.clone()),
            state_dir: directory.clone(),
            allow_insecure_http: true,
            persisted_allow_insecure_http: false,
            request_timeout_seconds: 1,
            ..AgentConfig::default()
        };
        let generation = Uuid::new_v4();
        let request_id = Uuid::new_v4();
        let activation_url =
            format!("http://127.0.0.1:1/modules/host-monitoring/activate/{request_id}");

        let creating = StoredPairingState::Creating {
            version: PAIRING_STATE_VERSION,
            generation,
            pairing_endpoint: pairing_endpoint.clone(),
            report_endpoint: report_endpoint.clone(),
            host: test_host(),
            bearer_secret: random_secret(),
            polling_secret: random_secret(),
        };
        let create_error = finish_create_request(&config, creating)
            .await
            .expect_err("a transient override must not resume durable Creating state");
        assert!(
            format!("{create_error:#}")
                .contains("requires allow_insecure_http=true in the existing persistent config")
        );

        let pending = StoredPairingState::Pending {
            version: PAIRING_STATE_VERSION,
            generation,
            request_id,
            activation_url: activation_url.clone(),
            expires_at: Utc::now() + TimeDelta::minutes(10),
            poll_interval: 1,
            pairing_endpoint: pairing_endpoint.clone(),
            report_endpoint: report_endpoint.clone(),
            bearer_secret: random_secret(),
            polling_secret: random_secret(),
        };
        persist_state(&config, &pending).unwrap();
        let activation_error = activate_pending_with_code(
            &config,
            generation,
            request_id,
            "uci_test_authorization_key",
        )
        .await
        .expect_err("a transient override must not activate durable Pending state");
        assert!(
            format!("{activation_error:#}")
                .contains("requires allow_insecure_http=true in the existing persistent config")
        );
        let polling_error = poll_existing(&config)
            .await
            .expect_err("a transient override must not poll durable Pending state");
        assert!(
            format!("{polling_error:#}")
                .contains("requires allow_insecure_http=true in the existing persistent config")
        );

        let instance_id = Uuid::new_v4();
        let activating = StoredPairingState::Activating {
            version: PAIRING_STATE_VERSION,
            generation,
            request_id,
            activation_url,
            expires_at: Utc::now() + TimeDelta::minutes(10),
            poll_interval: 1,
            instance_id,
            pairing_endpoint,
            report_endpoint: report_endpoint.clone(),
            bearer_secret: random_secret(),
        };
        let commit_error = finish_activating_unlocked(&config, activating)
            .expect_err("a transient override must not commit durable Activating state");
        assert!(
            format!("{commit_error:#}")
                .contains("requires allow_insecure_http=true in the existing persistent config")
        );
        assert!(!directory.join("agent-token").exists());
        assert!(!directory.join("host-id").exists());
        assert!(!active_binding_path(&config).exists());

        let binding = ActiveBinding {
            version: PAIRING_STATE_VERSION,
            generation,
            request_id,
            instance_id,
            report_endpoint,
        };
        assert!(validate_active_binding(&config, &binding).is_err());
        let mut durable_config = config.clone();
        durable_config.persisted_allow_insecure_http = true;
        validate_active_binding(&durable_config, &binding).unwrap();

        fs::remove_dir_all(directory).unwrap();
    }

    fn one_shot_pairing_server() -> (String, thread::JoinHandle<()>) {
        one_shot_pairing_server_with_activation_url(|request_id| {
            format!("/modules/host-monitoring/activate/{request_id}")
        })
    }

    fn one_shot_pairing_server_with_activation_url(
        activation_url: impl FnOnce(Uuid) -> String + Send + 'static,
    ) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let request_id = Uuid::new_v4();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut request = [0_u8; 16 * 1024];
            let read = stream.read(&mut request).unwrap();
            assert!(
                std::str::from_utf8(&request[..read])
                    .unwrap()
                    .starts_with("POST /api/modules/host-monitoring/agent/v2/pairing-requests ")
            );
            let body = serde_json::to_vec(&serde_json::json!({
                "request_id": request_id,
                "activation_url": activation_url(request_id),
                "expires_in": 600,
                "poll_interval": 1
            }))
            .unwrap();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(&body).unwrap();
            stream.flush().unwrap();
        });
        (format!("http://{address}"), handle)
    }

    #[tokio::test]
    async fn create_rejects_cross_origin_activation_url_before_showing_or_persisting_it() {
        let directory = std::env::temp_dir().join(format!(
            "unionc-pairing-untrusted-activation-{}",
            Uuid::new_v4()
        ));
        fs::create_dir_all(&directory).unwrap();
        let (server, server_thread) = one_shot_pairing_server_with_activation_url(|request_id| {
            format!("https://attacker.example/modules/host-monitoring/activate/{request_id}")
        });
        let config = AgentConfig {
            endpoint: format!("{server}/api/modules/host-monitoring/agent/v1/report"),
            pairing_endpoint: Some(format!(
                "{server}/api/modules/host-monitoring/agent/v2/pairing-requests"
            )),
            state_dir: directory.clone(),
            ..AgentConfig::default()
        };

        let error = start_or_resume(&config, &test_host())
            .await
            .expect_err("an untrusted browser destination must fail during request creation");
        assert!(error.to_string().contains("does not match"));
        assert!(matches!(
            load_state(&config).unwrap(),
            Some(StoredPairingState::Creating { .. })
        ));

        server_thread.join().unwrap();
        fs::remove_dir_all(directory).unwrap();
    }

    fn delayed_active_server(
        instance_id: Uuid,
    ) -> (
        String,
        mpsc::Receiver<()>,
        mpsc::Sender<()>,
        thread::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let (seen_tx, seen_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut request = [0_u8; 16 * 1024];
            let read = stream.read(&mut request).unwrap();
            assert!(
                std::str::from_utf8(&request[..read])
                    .unwrap()
                    .contains("/status ")
            );
            seen_tx.send(()).unwrap();
            release_rx
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap();
            let body = serde_json::to_vec(&serde_json::json!({
                "status": "active",
                "instance_id": instance_id
            }))
            .unwrap();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(&body).unwrap();
            stream.flush().unwrap();
        });
        (format!("http://{address}"), seen_rx, release_tx, handle)
    }

    fn delayed_activation_server(
        instance_id: Uuid,
    ) -> (
        String,
        mpsc::Receiver<()>,
        mpsc::Sender<()>,
        thread::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let (seen_tx, seen_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut request = [0_u8; 16 * 1024];
            let read = stream.read(&mut request).unwrap();
            assert!(
                std::str::from_utf8(&request[..read])
                    .unwrap()
                    .starts_with("POST /api/modules/host-monitoring/agent/v2/activate ")
            );
            seen_tx.send(()).unwrap();
            release_rx
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap();
            let body = serde_json::to_vec(&serde_json::json!({
                "status": "active",
                "instance_id": instance_id
            }))
            .unwrap();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(&body).unwrap();
            stream.flush().unwrap();
        });
        (format!("http://{address}"), seen_rx, release_tx, handle)
    }

    #[test]
    fn generated_secrets_have_256_bits_and_hash_the_transmitted_form() {
        let secret = random_secret();
        assert_eq!(secret.len(), 64);
        assert!(secret.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_eq!(sha256_hex(&secret).len(), 64);
        assert_ne!(secret, sha256_hex(&secret));
    }

    #[test]
    fn create_request_contract_contains_hashes_but_not_raw_secrets() {
        let host = HostIdentity {
            id: Uuid::new_v4().to_string(),
            os: "test".into(),
            os_version: None,
            kernel_version: None,
            arch: "test".into(),
            agent_version: "test".into(),
        };
        let bearer_secret = random_secret();
        let polling_secret = random_secret();
        let value = serde_json::to_value(CreatePairingRequest {
            host,
            token_hash: sha256_hex(&bearer_secret),
            polling_secret_hash: sha256_hex(&polling_secret),
        })
        .unwrap();
        let object = value.as_object().unwrap();
        assert_eq!(object.len(), 3);
        assert!(object.contains_key("host"));
        assert_eq!(object["token_hash"], sha256_hex(&bearer_secret));
        assert_eq!(object["polling_secret_hash"], sha256_hex(&polling_secret));
        let serialized = serde_json::to_string(&value).unwrap();
        assert!(!serialized.contains(&bearer_secret));
        assert!(!serialized.contains(&polling_secret));
    }

    #[test]
    fn status_contract_accepts_only_current_waiting_value() {
        let response: PairingStatusResponse = serde_json::from_value(serde_json::json!({
            "status": "waiting"
        }))
        .unwrap();
        assert!(matches!(response.status, PairingStatus::Waiting));
        assert!(response.instance_id.is_none());
        assert!(
            serde_json::from_value::<PairingStatusResponse>(serde_json::json!({
                "status": "pending"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<PairingStatusResponse>(serde_json::json!({
                "status": "waiting",
                "pending": true
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<PairingStatusResponse>(serde_json::json!({
                "status": "active",
                "instance_id": Uuid::new_v4().to_string().to_uppercase()
            }))
            .is_err()
        );
    }

    #[test]
    fn current_pairing_responses_and_local_auth_state_reject_unknown_fields() {
        assert!(
            serde_json::from_value::<CreatePairingResponse>(serde_json::json!({
                "request_id": Uuid::new_v4(),
                "activation_url": "https://unionc.example/agent/activate/request",
                "expires_in": 300,
                "poll_interval": 2,
                "enrollment_secret": "obsolete"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<CreatePairingResponse>(serde_json::json!({
                "request_id": Uuid::new_v4().to_string().replace('-', ""),
                "activation_url": "https://unionc.example/agent/activate/request",
                "expires_in": 300,
                "poll_interval": 2
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<ActivatePairingResponse>(serde_json::json!({
                "instance_id": Uuid::new_v4(),
                "status": "active",
                "token": "obsolete"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<ActivatePairingResponse>(serde_json::json!({
                "instance_id": Uuid::new_v4(),
                "status": "pending"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<ActivatePairingResponse>(serde_json::json!({
                "instance_id": Uuid::new_v4().to_string().to_uppercase(),
                "status": "active"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<LocalAuthState>(serde_json::json!({
                "version": env!("CARGO_PKG_VERSION"),
                "status": "authorized",
                "reason": "browser pairing completed",
                "changed_at": Utc::now(),
                "legacy": true
            }))
            .is_err()
        );
        for version in [
            serde_json::Value::Null,
            serde_json::json!(1),
            serde_json::json!("0.1.0"),
        ] {
            let mut state = serde_json::json!({
                "version": env!("CARGO_PKG_VERSION"),
                "status": "authorized",
                "reason": "browser pairing completed",
                "changed_at": Utc::now()
            });
            if version.is_null() {
                state.as_object_mut().unwrap().remove("version");
            } else {
                state["version"] = version;
            }
            assert!(serde_json::from_value::<LocalAuthState>(state).is_err());
        }
    }

    #[test]
    fn non_json_success_points_to_the_server_origin_without_leaking_the_body() {
        let endpoint = "http://127.0.0.1/api/modules/host-monitoring/agent/v2/pairing-requests";
        let body = b"<!doctype html><title>POETIZE private marker</title>";
        let error = parse_pairing_json::<CreatePairingResponse>(
            body,
            "text/html; charset=utf-8",
            endpoint,
            "pairing response",
        )
        .expect_err("HTML must not be accepted as a pairing response");
        let rendered = format!("{error:#}");
        assert!(rendered.contains("Server origin http://127.0.0.1"));
        assert!(!rendered.contains("/api/modules/host-monitoring/agent/v2/pairing-requests"));
        assert!(rendered.contains("Content-Type: text/html"));
        assert!(rendered.contains("address or port may be wrong"));
        assert!(rendered.contains("including its port"));
        assert!(!rendered.contains("POETIZE"));
        assert!(!rendered.contains("private marker"));

        let valid_json = serde_json::to_vec(&serde_json::json!({
            "request_id": Uuid::new_v4(),
            "activation_url": "/agent/activate/request",
            "expires_in": 600,
            "poll_interval": 2
        }))
        .unwrap();
        assert!(
            parse_pairing_json::<CreatePairingResponse>(
                &valid_json,
                "text/plain",
                endpoint,
                "pairing response"
            )
            .is_err()
        );
        assert!(
            parse_pairing_json::<CreatePairingResponse>(
                &valid_json,
                "application/vnd.unionc+json",
                endpoint,
                "pairing response"
            )
            .is_err()
        );
    }

    #[test]
    fn pairing_operations_accept_only_their_current_http_statuses() {
        assert!(
            ensure_pairing_status(
                StatusCode::OK,
                &[StatusCode::OK, StatusCode::CREATED],
                b"",
                "create pairing request"
            )
            .is_ok()
        );
        assert!(
            ensure_pairing_status(
                StatusCode::CREATED,
                &[StatusCode::OK, StatusCode::CREATED],
                b"",
                "create pairing request"
            )
            .is_ok()
        );
        for operation in [
            "poll pairing status",
            "submit the one-time authorization key",
        ] {
            assert!(
                ensure_pairing_status(StatusCode::OK, &[StatusCode::OK], b"", operation).is_ok()
            );
            assert!(
                ensure_pairing_status(StatusCode::NO_CONTENT, &[StatusCode::OK], b"", operation)
                    .is_err()
            );
        }
    }

    #[test]
    fn malformed_json_source_and_endpoint_secrets_are_fully_redacted() {
        let marker = "uci_SECRET_MARKER_MUST_NOT_LEAK";
        let body = format!(r#"{{"status":"{marker}"}}"#);
        let endpoint = format!(
            "https://user:{marker}@unionc.example/api/modules/host-monitoring/agent/v2/pairing-requests?key={marker}#{marker}"
        );
        let error = parse_pairing_json::<PairingStatusResponse>(
            body.as_bytes(),
            "application/json",
            &endpoint,
            "pairing status response",
        )
        .expect_err("an unknown status must not be accepted");
        let rendered = format!("{error:#}");
        assert!(rendered.contains("Server origin https://unionc.example"));
        assert!(rendered.contains("Content-Type: application/json"));
        assert!(!rendered.contains(marker));
        assert!(!rendered.contains("unknown variant"));
        assert!(!rendered.contains("Caused by"));
    }

    #[test]
    fn diagnostic_content_type_does_not_echo_parameters_or_unknown_values() {
        let marker = "uci_SECRET_MARKER_MUST_NOT_LEAK";
        assert_eq!(
            pairing_content_type_for_diagnostics(&format!("text/html; reflected={marker}")),
            "text/html"
        );
        assert_eq!(
            pairing_content_type_for_diagnostics(&format!("application/{marker}")),
            "<unexpected>"
        );
    }

    #[test]
    fn relative_activation_url_is_resolved_to_the_console_origin() {
        assert_eq!(
            resolve_activation_url(
                "https://unionc.example/api/modules/host-monitoring/agent/v2/pairing-requests",
                "/modules/host-monitoring/activate/00000000-0000-4000-8000-000000000001",
            )
            .unwrap(),
            "https://unionc.example/modules/host-monitoring/activate/00000000-0000-4000-8000-000000000001"
        );
    }

    #[test]
    fn insecure_override_never_applies_to_remote_activation_pages() {
        assert!(
            resolve_activation_url(
                "http://192.0.2.10/api/modules/host-monitoring/agent/v2/pairing-requests",
                "/modules/host-monitoring/activate/00000000-0000-4000-8000-000000000001",
            )
            .is_err()
        );
        assert!(
            resolve_activation_url(
                "http://127.0.0.1:8081/api/modules/host-monitoring/agent/v2/pairing-requests",
                "/modules/host-monitoring/activate/00000000-0000-4000-8000-000000000001",
            )
            .is_ok()
        );
        assert!(
            resolve_activation_url(
                "http://[::1]:8081/api/modules/host-monitoring/agent/v2/pairing-requests",
                "/modules/host-monitoring/activate/00000000-0000-4000-8000-000000000001",
            )
            .is_ok()
        );
    }

    #[test]
    fn activation_endpoint_and_public_url_stay_bound_to_the_pairing_origin() {
        let request_id = Uuid::new_v4();
        assert_eq!(
            activation_endpoint("https://unionc.example/prefix/api/modules/host-monitoring/agent/v2/pairing-requests")
                .unwrap()
                .as_str(),
            "https://unionc.example/prefix/api/modules/host-monitoring/agent/v2/activate"
        );
        validate_activation_url_request(
            &format!("https://unionc.example/modules/host-monitoring/activate/{request_id}"),
            "https://unionc.example/prefix/api/modules/host-monitoring/agent/v2/pairing-requests",
            request_id,
        )
        .unwrap();
        assert!(
            validate_activation_url_request(
                &format!("https://attacker.example/modules/host-monitoring/activate/{request_id}"),
                "https://unionc.example/api/modules/host-monitoring/agent/v2/pairing-requests",
                request_id,
            )
            .is_err()
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn service_activation_commit_wins_the_post_response_race_idempotently() {
        let directory =
            std::env::temp_dir().join(format!("unionc-activation-race-{}", Uuid::new_v4()));
        fs::create_dir_all(&directory).unwrap();
        let instance_id = Uuid::new_v4();
        let (server, request_seen, release_response, server_thread) =
            delayed_activation_server(instance_id);
        let generation = Uuid::new_v4();
        let request_id = Uuid::new_v4();
        let config = AgentConfig {
            endpoint: format!("{server}/api/modules/host-monitoring/agent/v1/report"),
            pairing_endpoint: Some(format!(
                "{server}/api/modules/host-monitoring/agent/v2/pairing-requests"
            )),
            state_dir: directory.clone(),
            allow_insecure_http: true,
            ..AgentConfig::default()
        };
        persist_state(
            &config,
            &StoredPairingState::Pending {
                version: PAIRING_STATE_VERSION,
                generation,
                request_id,
                activation_url: format!("{server}/modules/host-monitoring/activate/{request_id}"),
                expires_at: Utc::now() + TimeDelta::minutes(10),
                poll_interval: 1,
                pairing_endpoint: config.pairing_endpoint(),
                report_endpoint: config.endpoint.clone(),
                bearer_secret: random_secret(),
                polling_secret: random_secret(),
            },
        )
        .unwrap();

        let activation_config = config.clone();
        let activation = tokio::spawn(async move {
            activate_pending_with_code(
                &activation_config,
                generation,
                request_id,
                "uci_test_authorization_key",
            )
            .await
        });
        request_seen
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
        persist_state(
            &config,
            &StoredPairingState::Active {
                version: PAIRING_STATE_VERSION,
                generation,
                request_id,
                activation_url: format!("{server}/modules/host-monitoring/activate/{request_id}"),
                instance_id,
                report_endpoint: config.endpoint.clone(),
                completed_at: Utc::now(),
            },
        )
        .unwrap();
        release_response.send(()).unwrap();
        assert_eq!(activation.await.unwrap().unwrap(), Some(instance_id));
        server_thread.join().unwrap();
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn pending_state_round_trips_privately() {
        let directory = std::env::temp_dir().join(format!("unionc-pairing-{}", Uuid::new_v4()));
        let config = test_config(directory.clone());
        let state = StoredPairingState::Pending {
            version: PAIRING_STATE_VERSION,
            generation: Uuid::new_v4(),
            request_id: Uuid::new_v4(),
            activation_url: "https://unionc.example/agent/activate/test".into(),
            expires_at: Utc::now(),
            poll_interval: 5,
            pairing_endpoint: config.pairing_endpoint(),
            report_endpoint: config.endpoint.clone(),
            bearer_secret: random_secret(),
            polling_secret: random_secret(),
        };
        persist_state(&config, &state).unwrap();
        assert!(matches!(
            load_state(&config).unwrap(),
            Some(StoredPairingState::Pending { .. })
        ));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(state_path(&config))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn creating_state_round_trips_the_same_secrets_for_idempotent_retry() {
        let directory = std::env::temp_dir().join(format!("unionc-creating-{}", Uuid::new_v4()));
        let config = test_config(directory.clone());
        let bearer_secret = random_secret();
        let polling_secret = random_secret();
        let state = StoredPairingState::Creating {
            version: PAIRING_STATE_VERSION,
            generation: Uuid::new_v4(),
            pairing_endpoint: config.pairing_endpoint(),
            report_endpoint: config.endpoint.clone(),
            host: HostIdentity {
                id: Uuid::new_v4().to_string(),
                os: "test".into(),
                os_version: None,
                kernel_version: None,
                arch: "test".into(),
                agent_version: "test".into(),
            },
            bearer_secret: bearer_secret.clone(),
            polling_secret: polling_secret.clone(),
        };
        let mut encoded = serde_json::to_value(&state).unwrap();
        assert_eq!(encoded["version"], env!("CARGO_PKG_VERSION"));
        encoded["version"] = serde_json::json!(1);
        assert!(serde_json::from_value::<StoredPairingState>(encoded).is_err());
        persist_state(&config, &state).unwrap();
        assert!(matches!(
            load_state(&config).unwrap(),
            Some(StoredPairingState::Creating {
                bearer_secret: saved_bearer,
                polling_secret: saved_polling,
                ..
            }) if saved_bearer == bearer_secret && saved_polling == polling_secret
        ));
        fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn live_pending_request_cannot_be_silently_moved_to_another_server() {
        let directory =
            std::env::temp_dir().join(format!("unionc-pending-origin-{}", Uuid::new_v4()));
        let mut config = test_config(directory.clone());
        fs::create_dir_all(&directory).unwrap();
        let config_path = directory.join("config.json");
        config.config_path = Some(config_path.clone());
        let old_config = serde_json::to_vec(&config).unwrap();
        fs::write(&config_path, &old_config).unwrap();
        fs::write(directory.join("agent-token"), "existing-long-lived-token").unwrap();
        let state = StoredPairingState::Pending {
            version: PAIRING_STATE_VERSION,
            generation: Uuid::new_v4(),
            request_id: Uuid::new_v4(),
            activation_url: "https://old.example/agent/activate/test".into(),
            expires_at: Utc::now() + TimeDelta::minutes(10),
            poll_interval: 5,
            pairing_endpoint:
                "https://old.example/api/modules/host-monitoring/agent/v2/pairing-requests".into(),
            report_endpoint: "https://old.example/api/modules/host-monitoring/agent/v1/report"
                .into(),
            bearer_secret: random_secret(),
            polling_secret: random_secret(),
        };
        persist_state(&config, &state).unwrap();
        config.endpoint = "https://new.example/api/modules/host-monitoring/agent/v1/report".into();
        let error = start_or_resume(&config, &test_host())
            .await
            .expect_err("a live request must stay bound to its original server");
        assert!(error.to_string().contains("different UnionC server"));
        assert!(matches!(
            load_state(&config).unwrap(),
            Some(StoredPairingState::Pending { pairing_endpoint, .. })
                if pairing_endpoint.starts_with("https://old.example/")
        ));
        assert_eq!(fs::read(&config_path).unwrap(), old_config);
        assert_eq!(
            fs::read_to_string(directory.join("agent-token")).unwrap(),
            "existing-long-lived-token"
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn interrupted_create_cannot_be_silently_moved_to_another_server() {
        let directory =
            std::env::temp_dir().join(format!("unionc-creating-origin-{}", Uuid::new_v4()));
        let mut config = test_config(directory.clone());
        let state = StoredPairingState::Creating {
            version: PAIRING_STATE_VERSION,
            generation: Uuid::new_v4(),
            pairing_endpoint:
                "https://old.example/api/modules/host-monitoring/agent/v2/pairing-requests".into(),
            report_endpoint: "https://old.example/api/modules/host-monitoring/agent/v1/report"
                .into(),
            host: test_host(),
            bearer_secret: random_secret(),
            polling_secret: random_secret(),
        };
        persist_state(&config, &state).unwrap();
        config.endpoint = "https://new.example/api/modules/host-monitoring/agent/v1/report".into();
        let error = start_or_resume(&config, &test_host())
            .await
            .expect_err("an interrupted create must stay bound to its original server");
        assert!(error.to_string().contains("different UnionC server"));
        assert!(matches!(
            load_state(&config).unwrap(),
            Some(StoredPairingState::Creating { pairing_endpoint, .. })
                if pairing_endpoint.starts_with("https://old.example/")
        ));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn explicit_replacement_rotates_same_origin_incomplete_state() {
        for phase in ["creating", "expired_pending"] {
            let directory = std::env::temp_dir().join(format!(
                "unionc-same-origin-replace-{phase}-{}",
                Uuid::new_v4()
            ));
            let mut config = test_config(directory.clone());
            let old_generation = Uuid::new_v4();
            let old_bearer = random_secret();
            let old_polling = random_secret();
            let state = if phase == "creating" {
                StoredPairingState::Creating {
                    version: PAIRING_STATE_VERSION,
                    generation: old_generation,
                    pairing_endpoint: config.pairing_endpoint(),
                    report_endpoint: config.endpoint.clone(),
                    host: test_host(),
                    bearer_secret: old_bearer.clone(),
                    polling_secret: old_polling.clone(),
                }
            } else {
                let request_id = Uuid::new_v4();
                StoredPairingState::Pending {
                    version: PAIRING_STATE_VERSION,
                    generation: old_generation,
                    request_id,
                    activation_url: format!("https://unionc.example/agent/activate/{request_id}"),
                    expires_at: Utc::now() - TimeDelta::minutes(1),
                    poll_interval: 5,
                    pairing_endpoint: config.pairing_endpoint(),
                    report_endpoint: config.endpoint.clone(),
                    bearer_secret: old_bearer.clone(),
                    polling_secret: old_polling.clone(),
                }
            };
            persist_state(&config, &state).unwrap();

            match (phase, prepare_start(&config, &test_host()).unwrap()) {
                ("creating", PairingStart::Create(resumed)) => match *resumed {
                    StoredPairingState::Creating {
                        generation,
                        bearer_secret,
                        polling_secret,
                        ..
                    } => {
                        assert_eq!(generation, old_generation);
                        assert_eq!(bearer_secret, old_bearer);
                        assert_eq!(polling_secret, old_polling);
                    }
                    _ => panic!("creating state was not resumed"),
                },
                ("expired_pending", PairingStart::Waiting(session)) => {
                    assert_eq!(session.generation, old_generation);
                }
                _ => panic!("ordinary pairing did not conservatively resume saved state"),
            }

            config.replace_pending_pairing = true;
            let PairingStart::Create(replacement) = prepare_start(&config, &test_host()).unwrap()
            else {
                panic!("explicit replacement did not create a fresh generation");
            };
            let StoredPairingState::Creating {
                generation: new_generation,
                bearer_secret: new_bearer,
                polling_secret: new_polling,
                ..
            } = *replacement
            else {
                panic!("explicit replacement did not persist a creating state");
            };
            assert_ne!(new_generation, old_generation);
            assert_ne!(new_bearer, old_bearer);
            assert_ne!(new_polling, old_polling);
            assert!(matches!(
                load_state(&config).unwrap(),
                Some(StoredPairingState::Creating {
                    generation,
                    bearer_secret,
                    polling_secret,
                    ..
                }) if generation == new_generation
                    && bearer_secret == new_bearer
                    && polling_secret == new_polling
            ));
            fs::remove_dir_all(directory).unwrap();
        }
    }

    #[tokio::test]
    async fn confirmed_tray_replacement_can_replace_mismatched_incomplete_states() {
        for old_state in ["creating", "pending"] {
            let directory = std::env::temp_dir().join(format!(
                "unionc-confirmed-replace-{old_state}-{}",
                Uuid::new_v4()
            ));
            let (server, server_thread) = one_shot_pairing_server();
            let mut config = AgentConfig {
                endpoint: format!("{server}/api/modules/host-monitoring/agent/v1/report"),
                state_dir: directory.clone(),
                replace_pending_pairing: true,
                ..AgentConfig::default()
            };
            config.pairing_endpoint = Some(format!(
                "{server}/api/modules/host-monitoring/agent/v2/pairing-requests"
            ));
            let state = if old_state == "creating" {
                StoredPairingState::Creating {
                    version: PAIRING_STATE_VERSION,
                    generation: Uuid::new_v4(),
                    pairing_endpoint:
                        "https://old.example/api/modules/host-monitoring/agent/v2/pairing-requests"
                            .into(),
                    report_endpoint:
                        "https://old.example/api/modules/host-monitoring/agent/v1/report".into(),
                    host: test_host(),
                    bearer_secret: random_secret(),
                    polling_secret: random_secret(),
                }
            } else {
                StoredPairingState::Pending {
                    version: PAIRING_STATE_VERSION,
                    generation: Uuid::new_v4(),
                    request_id: Uuid::new_v4(),
                    activation_url: "https://old.example/agent/activate/test".into(),
                    expires_at: Utc::now() + TimeDelta::minutes(10),
                    poll_interval: 5,
                    pairing_endpoint:
                        "https://old.example/api/modules/host-monitoring/agent/v2/pairing-requests"
                            .into(),
                    report_endpoint:
                        "https://old.example/api/modules/host-monitoring/agent/v1/report".into(),
                    bearer_secret: random_secret(),
                    polling_secret: random_secret(),
                }
            };
            persist_state(&config, &state).unwrap();
            let session = start_or_resume(&config, &test_host())
                .await
                .expect("the explicitly confirmed new origin should replace incomplete state");
            assert!(session.activation_url.starts_with(&server));
            assert!(matches!(
                load_state(&config).unwrap(),
                Some(StoredPairingState::Pending { pairing_endpoint, .. })
                    if pairing_endpoint == format!("{server}/api/modules/host-monitoring/agent/v2/pairing-requests")
            ));
            server_thread.join().unwrap();
            fs::remove_dir_all(directory).unwrap();
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn delayed_old_activation_cannot_overwrite_a_replacement_generation() {
        let directory =
            std::env::temp_dir().join(format!("unionc-delayed-active-{}", Uuid::new_v4()));
        fs::create_dir_all(&directory).unwrap();
        let old_instance_id = Uuid::new_v4();
        let (old_server, request_seen, release_response, old_thread) =
            delayed_active_server(old_instance_id);
        let old_config_path = directory.join("config.json");
        let old_pairing_endpoint =
            format!("{old_server}/api/modules/host-monitoring/agent/v2/pairing-requests");
        let old_report_endpoint =
            format!("{old_server}/api/modules/host-monitoring/agent/v1/report");
        let old_config = AgentConfig {
            endpoint: old_report_endpoint.clone(),
            pairing_endpoint: Some(old_pairing_endpoint.clone()),
            state_dir: directory.clone(),
            config_path: Some(old_config_path.clone()),
            ..AgentConfig::default()
        };
        let old_config_bytes = serde_json::to_vec(&old_config).unwrap();
        fs::write(&old_config_path, &old_config_bytes).unwrap();
        fs::write(directory.join("agent-token"), "old-long-lived-token").unwrap();
        let old_host_id = Uuid::new_v4();
        fs::write(directory.join("host-id"), old_host_id.to_string()).unwrap();
        let generation = Uuid::new_v4();
        let request_id = Uuid::new_v4();
        let old_state = StoredPairingState::Pending {
            version: PAIRING_STATE_VERSION,
            generation,
            request_id,
            activation_url: format!("{old_server}/agent/activate/{request_id}"),
            expires_at: Utc::now() + TimeDelta::minutes(10),
            poll_interval: 1,
            pairing_endpoint: old_pairing_endpoint.clone(),
            report_endpoint: old_report_endpoint.clone(),
            bearer_secret: random_secret(),
            polling_secret: random_secret(),
        };
        persist_state(&old_config, &old_state).unwrap();
        let polling_config = old_config.clone();
        let stale_poll = tokio::spawn(async move { poll_existing(&polling_config).await });
        request_seen
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();

        let (new_server, new_thread) = one_shot_pairing_server();
        let mut new_config = AgentConfig {
            endpoint: format!("{new_server}/api/modules/host-monitoring/agent/v1/report"),
            pairing_endpoint: Some(format!(
                "{new_server}/api/modules/host-monitoring/agent/v2/pairing-requests"
            )),
            state_dir: directory.clone(),
            config_path: Some(old_config_path.clone()),
            replace_pending_pairing: true,
            ..AgentConfig::default()
        };
        new_config.allow_insecure_http = true;
        let new_session = start_or_resume(&new_config, &test_host()).await.unwrap();
        release_response.send(()).unwrap();
        let stale_error = stale_poll
            .await
            .unwrap()
            .expect_err("the delayed old Active response must lose its generation CAS");
        assert!(stale_error.is::<PairingSuperseded>());
        assert!(matches!(
            load_state(&new_config).unwrap(),
            Some(StoredPairingState::Pending {
                generation: saved_generation,
                pairing_endpoint,
                ..
            }) if saved_generation == new_session.generation
                && pairing_endpoint.starts_with(&new_server)
        ));
        assert_eq!(
            fs::read_to_string(directory.join("agent-token")).unwrap(),
            "old-long-lived-token"
        );
        assert_eq!(
            fs::read_to_string(directory.join("host-id")).unwrap(),
            old_host_id.to_string()
        );
        assert_eq!(fs::read(&old_config_path).unwrap(), old_config_bytes);
        old_thread.join().unwrap();
        new_thread.join().unwrap();
        fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn activating_journal_recovers_all_endpoint_bound_files() {
        for preexisting in [false, true] {
            let directory = std::env::temp_dir().join(format!(
                "unionc-activating-recovery-{preexisting}-{}",
                Uuid::new_v4()
            ));
            fs::create_dir_all(&directory).unwrap();
            // Model an administrator-owned system config that the service cannot replace. A
            // directory is deterministic even when this test happens to run as root.
            let config_path = directory.join("operator-config");
            fs::create_dir(&config_path).unwrap();
            let mut config = AgentConfig {
                endpoint: "https://old.example/api/modules/host-monitoring/agent/v1/report".into(),
                state_dir: directory.clone(),
                config_path: Some(config_path.clone()),
                ..AgentConfig::default()
            };
            if preexisting {
                fs::write(directory.join("agent-token"), "old-token").unwrap();
                fs::write(directory.join("host-id"), Uuid::new_v4().to_string()).unwrap();
            }
            let generation = Uuid::new_v4();
            let request_id = Uuid::new_v4();
            let instance_id = Uuid::new_v4();
            let new_token = random_secret();
            persist_state(
                &config,
                &StoredPairingState::Activating {
                    version: PAIRING_STATE_VERSION,
                    generation,
                    request_id,
                    activation_url: "https://new.example/agent/activate/test".into(),
                    expires_at: Utc::now() + TimeDelta::minutes(10),
                    poll_interval: 1,
                    instance_id,
                    pairing_endpoint:
                        "https://new.example/api/modules/host-monitoring/agent/v2/pairing-requests"
                            .into(),
                    report_endpoint:
                        "https://new.example/api/modules/host-monitoring/agent/v1/report".into(),
                    bearer_secret: new_token.clone(),
                },
            )
            .unwrap();

            let progress = poll_existing(&config).await.unwrap().unwrap();
            assert!(matches!(
                progress,
                PairingProgress::Active {
                    generation: saved_generation,
                    request_id: saved_request,
                    instance_id: saved_instance,
                    ..
                } if saved_generation == generation
                    && saved_request == request_id
                    && saved_instance == instance_id
            ));
            assert_eq!(
                fs::read_to_string(directory.join("agent-token")).unwrap(),
                new_token
            );
            assert_eq!(
                fs::read_to_string(directory.join("host-id")).unwrap(),
                instance_id.to_string()
            );
            assert_eq!(
                load_active_binding(&config).unwrap(),
                Some(ActiveBinding {
                    version: PAIRING_STATE_VERSION,
                    generation,
                    request_id,
                    instance_id,
                    report_endpoint:
                        "https://new.example/api/modules/host-monitoring/agent/v1/report".into(),
                })
            );
            let binding_before_status = fs::read(active_binding_path(&config)).unwrap();
            let status = local_status(&config).unwrap();
            assert_eq!(
                status.active_report_endpoint.as_deref(),
                Some("https://new.example/api/modules/host-monitoring/agent/v1/report")
            );
            assert!(status.active_binding_persisted);
            assert_eq!(
                fs::read(active_binding_path(&config)).unwrap(),
                binding_before_status
            );
            assert!(config_path.is_dir());
            assert!(matches!(
                load_state(&config).unwrap(),
                Some(StoredPairingState::Active {
                    generation: saved_generation,
                    ..
                }) if saved_generation == generation
            ));
            let mut host = test_host();
            activate_reporter_snapshot(
                &mut config,
                &mut host,
                generation,
                request_id,
                instance_id,
                "https://new.example/api/modules/host-monitoring/agent/v1/report",
            )
            .unwrap();
            assert_eq!(
                config.endpoint,
                "https://new.example/api/modules/host-monitoring/agent/v1/report"
            );
            assert!(config_path.is_dir());
            assert!(matches!(
                poll_existing(&config).await.unwrap(),
                Some(PairingProgress::Active {
                    generation: saved_generation,
                    ..
                }) if saved_generation == generation
            ));
            fs::remove_dir_all(directory).unwrap();
        }
    }

    #[test]
    fn old_active_state_lazily_migrates_its_endpoint_binding() {
        let directory =
            std::env::temp_dir().join(format!("unionc-binding-migration-{}", Uuid::new_v4()));
        let config = test_config(directory.clone());
        fs::create_dir_all(&directory).unwrap();
        let generation = Uuid::new_v4();
        let request_id = Uuid::new_v4();
        let instance_id = Uuid::new_v4();
        fs::write(directory.join("agent-token"), "current-token").unwrap();
        fs::write(directory.join("host-id"), instance_id.to_string()).unwrap();
        persist_auth_state(
            &config,
            &LocalAuthState {
                version: PAIRING_STATE_VERSION,
                status: "authorized".into(),
                reason: "existing installation".into(),
                changed_at: Utc::now(),
            },
        )
        .unwrap();
        persist_state(
            &config,
            &StoredPairingState::Active {
                version: PAIRING_STATE_VERSION,
                generation,
                request_id,
                activation_url: "https://unionc.example/agent/activate/test".into(),
                instance_id,
                report_endpoint: config.endpoint.clone(),
                completed_at: Utc::now(),
            },
        )
        .unwrap();

        let state_before_status = fs::read(state_path(&config)).unwrap();
        assert!(!active_binding_path(&config).exists());
        let status = local_status(&config).unwrap();
        assert_eq!(
            status.active_report_endpoint.as_deref(),
            Some(config.endpoint.as_str())
        );
        assert!(!status.active_binding_persisted);
        assert_eq!(fs::read(state_path(&config)).unwrap(), state_before_status);
        assert!(!active_binding_path(&config).exists());
        assert!(
            reporter_for_current_active_state(&config)
                .unwrap()
                .is_some()
        );
        assert_eq!(
            load_active_binding(&config).unwrap(),
            Some(ActiveBinding {
                version: PAIRING_STATE_VERSION,
                generation,
                request_id,
                instance_id,
                report_endpoint: config.endpoint.clone(),
            })
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn mismatched_active_binding_is_never_silently_replaced() {
        let directory =
            std::env::temp_dir().join(format!("unionc-binding-mismatch-{}", Uuid::new_v4()));
        let mut config = test_config(directory.clone());
        fs::create_dir_all(&directory).unwrap();
        let generation = Uuid::new_v4();
        let request_id = Uuid::new_v4();
        let instance_id = Uuid::new_v4();
        fs::write(directory.join("agent-token"), "current-token").unwrap();
        fs::write(directory.join("host-id"), instance_id.to_string()).unwrap();
        persist_auth_state(
            &config,
            &LocalAuthState {
                version: PAIRING_STATE_VERSION,
                status: "authorized".into(),
                reason: "test".into(),
                changed_at: Utc::now(),
            },
        )
        .unwrap();
        persist_state(
            &config,
            &StoredPairingState::Active {
                version: PAIRING_STATE_VERSION,
                generation,
                request_id,
                activation_url: "https://unionc.example/agent/activate/test".into(),
                instance_id,
                report_endpoint: config.endpoint.clone(),
                completed_at: Utc::now(),
            },
        )
        .unwrap();
        let mismatched = ActiveBinding {
            version: PAIRING_STATE_VERSION,
            generation: Uuid::new_v4(),
            request_id,
            instance_id,
            report_endpoint: config.endpoint.clone(),
        };
        persist_active_binding_unlocked(&config, &mismatched).unwrap();

        let status_error =
            local_status(&config).expect_err("status must reject a mismatched binding");
        assert!(status_error.to_string().contains("does not match"));
        let reporter_error = match reporter_for_current_active_state(&config) {
            Ok(_) => panic!("a mismatched binding must fail closed"),
            Err(error) => error,
        };
        assert!(reporter_error.to_string().contains("does not match"));
        let config_error = commit_active_configuration(
            &mut config,
            generation,
            request_id,
            instance_id,
            "https://unionc.example/api/modules/host-monitoring/agent/v1/report",
        )
        .expect_err("config synchronization must not replace a mismatched binding");
        assert!(config_error.to_string().contains("does not match"));
        assert_eq!(load_active_binding(&config).unwrap(), Some(mismatched));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn replacing_old_active_state_preserves_its_endpoint_binding() {
        let directory =
            std::env::temp_dir().join(format!("unionc-binding-before-create-{}", Uuid::new_v4()));
        let mut config = test_config(directory.clone());
        let old_generation = Uuid::new_v4();
        let old_request_id = Uuid::new_v4();
        let old_instance_id = Uuid::new_v4();
        let old_endpoint = config.endpoint.clone();
        persist_state(
            &config,
            &StoredPairingState::Active {
                version: PAIRING_STATE_VERSION,
                generation: old_generation,
                request_id: old_request_id,
                activation_url: "https://unionc.example/agent/activate/old".into(),
                instance_id: old_instance_id,
                report_endpoint: old_endpoint.clone(),
                completed_at: Utc::now(),
            },
        )
        .unwrap();
        config.endpoint = "https://new.example/api/modules/host-monitoring/agent/v1/report".into();
        config.pairing_endpoint = Some(
            "https://new.example/api/modules/host-monitoring/agent/v2/pairing-requests".into(),
        );

        let PairingStart::Create(creating) = prepare_start(&config, &test_host()).unwrap() else {
            panic!("an Active state must allow a new explicitly requested pairing generation");
        };
        assert!(matches!(
            *creating,
            StoredPairingState::Creating { ref report_endpoint, .. }
                if report_endpoint == "https://new.example/api/modules/host-monitoring/agent/v1/report"
        ));
        assert_eq!(
            load_active_binding(&config).unwrap(),
            Some(ActiveBinding {
                version: PAIRING_STATE_VERSION,
                generation: old_generation,
                request_id: old_request_id,
                instance_id: old_instance_id,
                report_endpoint: old_endpoint,
            })
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn run_keeps_the_current_credential_during_an_incomplete_pairing_attempt() {
        let directory =
            std::env::temp_dir().join(format!("unionc-current-reporter-{}", Uuid::new_v4()));
        let config = test_config(directory.clone());
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("agent-token"), "current-long-lived-token").unwrap();
        let generation = Uuid::new_v4();
        let request_id = Uuid::new_v4();
        let states = [
            StoredPairingState::Creating {
                version: PAIRING_STATE_VERSION,
                generation,
                pairing_endpoint: config.pairing_endpoint(),
                report_endpoint: config.endpoint.clone(),
                host: test_host(),
                bearer_secret: random_secret(),
                polling_secret: random_secret(),
            },
            StoredPairingState::Pending {
                version: PAIRING_STATE_VERSION,
                generation,
                request_id,
                activation_url: "https://unionc.example/agent/activate/test".into(),
                expires_at: Utc::now() + TimeDelta::minutes(10),
                poll_interval: 5,
                pairing_endpoint: config.pairing_endpoint(),
                report_endpoint: config.endpoint.clone(),
                bearer_secret: random_secret(),
                polling_secret: random_secret(),
            },
        ];
        persist_state(&config, &states[1]).unwrap();
        assert!(
            existing_reporter_for_run(&config).unwrap().is_none(),
            "a token and pairing journal without current authorized state must be rejected"
        );
        persist_auth_state(
            &config,
            &LocalAuthState {
                version: PAIRING_STATE_VERSION,
                status: "authorized".into(),
                reason: "current pairing completed".into(),
                changed_at: Utc::now(),
            },
        )
        .unwrap();
        for state in states {
            persist_state(&config, &state).unwrap();
            assert!(existing_reporter_for_run(&config).unwrap().is_some());
            assert!(has_current_authorized_identity(&config).unwrap());
        }

        persist_state(
            &config,
            &StoredPairingState::Denied {
                version: PAIRING_STATE_VERSION,
                generation,
                request_id,
                activation_url: "https://unionc.example/agent/activate/test".into(),
                report_endpoint: config.endpoint.clone(),
                completed_at: Utc::now(),
            },
        )
        .unwrap();
        assert!(
            existing_reporter_for_run(&config).unwrap().is_some(),
            "a denied pairing attempt must not discard the still-authorized credential"
        );
        assert!(has_current_authorized_identity(&config).unwrap());

        persist_state(
            &config,
            &StoredPairingState::Expired {
                version: PAIRING_STATE_VERSION,
                generation,
                request_id,
                activation_url: "https://unionc.example/agent/activate/test".into(),
                report_endpoint: config.endpoint.clone(),
                completed_at: Utc::now(),
            },
        )
        .unwrap();
        assert!(
            existing_reporter_for_run(&config).unwrap().is_some(),
            "an expired pairing attempt must not discard the still-authorized credential"
        );
        assert!(has_current_authorized_identity(&config).unwrap());

        fs::remove_file(directory.join(PAIRING_STATE_FILE)).unwrap();
        assert!(
            existing_reporter_for_run(&config).unwrap().is_none(),
            "a raw token without current package-version pairing state must be rejected"
        );

        fs::write(directory.join("agent-token"), "active-token").unwrap();
        persist_state(
            &config,
            &StoredPairingState::Active {
                version: PAIRING_STATE_VERSION,
                generation,
                request_id,
                activation_url: "https://unionc.example/agent/activate/test".into(),
                instance_id: Uuid::new_v4(),
                report_endpoint: config.endpoint.clone(),
                completed_at: Utc::now(),
            },
        )
        .unwrap();
        assert!(existing_reporter_for_run(&config).unwrap().is_none());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn stale_delivery_cannot_block_a_new_active_generation() {
        let directory = std::env::temp_dir().join(format!("unionc-reauth-cas-{}", Uuid::new_v4()));
        let config = test_config(directory.clone());
        let old_generation = Uuid::new_v4();
        let old_request = Uuid::new_v4();
        let active = |generation, request_id| StoredPairingState::Active {
            version: PAIRING_STATE_VERSION,
            generation,
            request_id,
            activation_url: "https://unionc.example/agent/activate/test".into(),
            instance_id: Uuid::new_v4(),
            report_endpoint: config.endpoint.clone(),
            completed_at: Utc::now(),
        };
        persist_state(&config, &active(old_generation, old_request)).unwrap();
        assert!(
            mark_reauth_required_if_current(
                &config,
                Some((old_generation, old_request)),
                "current 401",
            )
            .unwrap()
        );

        persist_state(&config, &active(Uuid::new_v4(), Uuid::new_v4())).unwrap();
        assert!(
            !mark_reauth_required_if_current(
                &config,
                Some((old_generation, old_request)),
                "stale 403",
            )
            .unwrap()
        );

        persist_state(
            &config,
            &StoredPairingState::Pending {
                version: PAIRING_STATE_VERSION,
                generation: Uuid::new_v4(),
                request_id: Uuid::new_v4(),
                activation_url: "https://unionc.example/agent/activate/test".into(),
                expires_at: Utc::now() + TimeDelta::minutes(10),
                poll_interval: 5,
                pairing_endpoint: config.pairing_endpoint(),
                report_endpoint: config.endpoint.clone(),
                bearer_secret: random_secret(),
                polling_secret: random_secret(),
            },
        )
        .unwrap();
        assert!(
            mark_reauth_required_if_current(
                &config,
                Some((old_generation, old_request)),
                "old reporter rejected during pending pairing",
            )
            .unwrap()
        );

        persist_state(
            &config,
            &StoredPairingState::Activating {
                version: PAIRING_STATE_VERSION,
                generation: Uuid::new_v4(),
                request_id: Uuid::new_v4(),
                activation_url: "https://unionc.example/agent/activate/test".into(),
                expires_at: Utc::now() + TimeDelta::minutes(10),
                poll_interval: 5,
                instance_id: Uuid::new_v4(),
                pairing_endpoint: config.pairing_endpoint(),
                report_endpoint: config.endpoint.clone(),
                bearer_secret: random_secret(),
            },
        )
        .unwrap();
        assert!(
            !mark_reauth_required_if_current(
                &config,
                Some((old_generation, old_request)),
                "stale while new activation commits",
            )
            .unwrap()
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn rejected_authorization_state_is_explicit() {
        let directory = std::env::temp_dir().join(format!("unionc-rejected-{}", Uuid::new_v4()));
        let config = test_config(directory.clone());
        mark_reauth_required(&config, "HTTP 401 unauthorized").unwrap();
        let state = local_auth_state(&config).unwrap().unwrap();
        assert_eq!(state.status, "reauth_required");
        assert!(state.reason.contains("401"));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn local_inspection_does_not_create_a_lock_or_state_directory() {
        let directory =
            std::env::temp_dir().join(format!("unionc-read-only-status-{}", Uuid::new_v4()));
        let config = test_config(directory.clone());

        assert!(local_progress(&config).unwrap().is_none());
        assert!(local_auth_state(&config).unwrap().is_none());
        assert!(
            !directory.exists(),
            "read-only status inspection must not create the state directory"
        );
    }

    #[test]
    fn local_inspection_does_not_publish_an_activating_credential() {
        let directory =
            std::env::temp_dir().join(format!("unionc-read-only-activating-{}", Uuid::new_v4()));
        let config = test_config(directory.clone());
        let state = StoredPairingState::Activating {
            version: PAIRING_STATE_VERSION,
            generation: Uuid::new_v4(),
            request_id: Uuid::new_v4(),
            activation_url: "https://unionc.example/agent/activate/test".into(),
            expires_at: Utc::now() + TimeDelta::minutes(10),
            poll_interval: 5,
            instance_id: Uuid::new_v4(),
            pairing_endpoint: config.pairing_endpoint(),
            report_endpoint: config.endpoint.clone(),
            bearer_secret: random_secret(),
        };
        persist_state(&config, &state).unwrap();
        let state_path = state_path(&config);
        let before = fs::read(&state_path).unwrap();

        assert!(matches!(
            local_progress(&config).unwrap(),
            Some(PairingProgress::Creating { .. })
        ));
        assert_eq!(fs::read(&state_path).unwrap(), before);
        assert!(!directory.join("agent-token").exists());
        assert!(!directory.join("host-id").exists());
        assert!(!directory.join("auth-state.json").exists());
        assert!(!active_binding_path(&config).exists());

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn activation_atomically_commits_server_identity_and_token() {
        let directory = std::env::temp_dir().join(format!("unionc-activation-{}", Uuid::new_v4()));
        let config = test_config(directory.clone());
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("host-id"), Uuid::new_v4().to_string()).unwrap();
        fs::write(directory.join("agent-token"), "old-token").unwrap();
        let instance_id = Uuid::new_v4();
        let bearer_secret = random_secret();
        let polling_secret = random_secret();
        let generation = Uuid::new_v4();
        let request_id = Uuid::new_v4();
        let pairing_endpoint = config.pairing_endpoint();
        persist_state(
            &config,
            &StoredPairingState::Pending {
                version: PAIRING_STATE_VERSION,
                generation,
                request_id,
                activation_url: "https://unionc.example/agent/activate/test".into(),
                expires_at: Utc::now() + TimeDelta::minutes(10),
                poll_interval: 5,
                pairing_endpoint: pairing_endpoint.clone(),
                report_endpoint: config.endpoint.clone(),
                bearer_secret: bearer_secret.clone(),
                polling_secret: polling_secret.clone(),
            },
        )
        .unwrap();

        persist_active_credentials(&config, load_state(&config).unwrap().unwrap(), instance_id)
            .unwrap();

        assert_eq!(
            fs::read_to_string(directory.join("host-id")).unwrap(),
            instance_id.to_string()
        );
        assert_eq!(
            fs::read_to_string(directory.join("agent-token")).unwrap(),
            bearer_secret
        );
        let binding: ActiveBinding =
            serde_json::from_slice(&fs::read(directory.join(ACTIVE_BINDING_FILE)).unwrap())
                .unwrap();
        assert_eq!(
            binding,
            ActiveBinding {
                version: PAIRING_STATE_VERSION,
                generation,
                request_id,
                instance_id,
                report_endpoint: config.endpoint.clone(),
            }
        );
        assert!(matches!(
            load_state(&config).unwrap(),
            Some(StoredPairingState::Active {
                instance_id: saved,
                ..
            }) if saved == instance_id
        ));
        assert_eq!(
            local_auth_state(&config).unwrap().unwrap().status,
            "authorized"
        );
        fs::remove_dir_all(directory).unwrap();
    }
}
