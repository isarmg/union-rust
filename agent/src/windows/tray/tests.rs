#[cfg(test)]
mod tests {
    use super::*;

    fn probe_health_body(body: &str) -> ServerConnectionStatus {
        probe_health_response("200 OK", Some("application/json"), body)
    }

    fn probe_health_response(
        status: &str,
        content_type: Option<&str>,
        body: &str,
    ) -> ServerConnectionStatus {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let content_type = content_type
            .map(|value| format!("Content-Type: {value}\r\n"))
            .unwrap_or_default();
        let response = format!(
            "HTTP/1.1 {status}\r\n{content_type}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let worker = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).unwrap();
            stream.write_all(response.as_bytes()).unwrap();
        });
        let result = probe_server_connection(&format!("http://{address}"));
        worker.join().unwrap();
        result
    }

    #[test]
    fn serde_dtos_accept_only_the_current_wire_shape() {
        let current_preferences = format!(
            r#"{{"application_version":"{}","server":""}}"#,
            env!("CARGO_PKG_VERSION")
        );
        assert!(serde_json::from_str::<TrayPreferences>(&current_preferences).is_ok());
        assert!(serde_json::from_str::<TrayPreferences>(r#"{"server":""}"#).is_err());
        assert!(
            serde_json::from_str::<TrayPreferences>(
                r#"{"application_version":"0.3.1","server":""}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<TrayPreferences>(
                r#"{"application_version":"0.4.0","server":"","legacy":true}"#
            )
            .is_err()
        );

        assert!(
            serde_json::from_str::<PairRequest>(
                r#"{"server":"https://server.example","activation_code":"secret"}"#
            )
            .is_ok()
        );
        assert!(
            serde_json::from_str::<PairRequest>(
                r#"{"server":"https://server.example","name":"host","activation_code":"secret"}"#
            )
            .is_err()
        );
        assert!(serde_json::from_str::<ConnectionRequest>(r#"{"server":""}"#).is_ok());
        assert!(serde_json::from_str::<ConnectionRequest>(r#"{}"#).is_err());
        assert!(serde_json::from_str::<StateRequest>(r#"{}"#).is_ok());
        assert!(serde_json::from_str::<StateRequest>(r#"{"legacy":true}"#).is_err());
        assert!(serde_json::from_str::<ServiceRequest>(r#"{"action":"start","extra":1}"#).is_err());
        assert!(serde_json::from_str::<OperationRequest>(r#"{"id":"id","extra":1}"#).is_err());

        assert!(
            serde_json::from_str::<PairIpcMessage>(
                r#"{"generation":"generation","request_id":"request","activation_url":"https://server.example/modules/host-monitoring/activate/request","pairing_endpoint":"https://server.example/api/modules/host-monitoring/agent/v2/pairing-requests"}"#
            )
            .is_ok()
        );
        assert!(
            serde_json::from_str::<PairIpcMessage>(
                r#"{"generation":"generation","request_id":"request","activation_url":"https://server.example/modules/host-monitoring/activate/request","pairing_endpoint":"https://server.example/api/modules/host-monitoring/agent/v2/pairing-requests","legacy":true}"#
            )
            .is_err()
        );

        let waiting_event = format!(
            r#"{{"event":"pairing_waiting","version":"{}","request_id":"request","generation":"generation","activation_url":"https://server.example/modules/host-monitoring/activate/request","pairing_endpoint":"https://server.example/api/modules/host-monitoring/agent/v2/pairing-requests","expires_at":"2026-08-20T00:00:00Z","poll_interval":2}}"#,
            env!("CARGO_PKG_VERSION")
        );
        assert!(serde_json::from_str::<PairEvent>(&waiting_event).is_ok());
        let waiting_event_without_expiry = format!(
            r#"{{"event":"pairing_waiting","version":"{}","request_id":"request","generation":"generation","activation_url":"https://server.example/modules/host-monitoring/activate/request","pairing_endpoint":"https://server.example/api/modules/host-monitoring/agent/v2/pairing-requests","poll_interval":2}}"#,
            env!("CARGO_PKG_VERSION")
        );
        assert!(serde_json::from_str::<PairEvent>(&waiting_event_without_expiry).is_err());
        let paired_event_with_legacy_field = format!(
            r#"{{"event":"paired","version":"{}","request_id":"request","instance_id":"instance","endpoint":"https://server.example/api/modules/host-monitoring/agent/v1/report","legacy":true}}"#,
            env!("CARGO_PKG_VERSION")
        );
        assert!(serde_json::from_str::<PairEvent>(&paired_event_with_legacy_field).is_err());
        assert!(
            serde_json::from_str::<PairEvent>(r#"{"event":"pairing_cancelled","version":1}"#)
                .is_err()
        );
        assert!(
            serde_json::from_str::<PairEvent>(r#"{"event":"pairing_cancelled","version":"0.3.1"}"#)
                .is_err()
        );

        let health_response = format!(
            r#"{{"status":"ok","version":"{}","uptime_seconds":1}}"#,
            env!("CARGO_PKG_VERSION")
        );
        assert!(serde_json::from_str::<ServerHealthResponse>(&health_response).is_ok());
        assert!(
            serde_json::from_str::<ServerHealthResponse>(r#"{"status":"ok","uptime_seconds":1}"#)
                .is_err()
        );
        let health_response_with_legacy_field = format!(
            r#"{{"status":"ok","version":"{}","uptime_seconds":1,"legacy":true}}"#,
            env!("CARGO_PKG_VERSION")
        );
        assert!(
            serde_json::from_str::<ServerHealthResponse>(&health_response_with_legacy_field)
                .is_err()
        );
    }

    #[test]
    fn existing_preferences_can_be_atomically_replaced() {
        let directory =
            std::env::temp_dir().join(format!("unionc-tray-preferences-{}", random_secret()));
        let path = directory.join("tray.json");
        let first = TrayPreferences {
            application_version: CurrentPackageVersion,
            server: "https://first.example".into(),
        };
        let second = TrayPreferences {
            application_version: CurrentPackageVersion,
            server: "https://second.example".into(),
        };
        save_preferences(&path, &first).unwrap();
        save_preferences(&path, &second).unwrap();
        let loaded = load_preferences(&path).unwrap();
        assert_eq!(loaded.server, second.server);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn pairing_ipc_requires_canonical_uuid_text() {
        let generation = uuid::Uuid::new_v4().to_string();
        let request_id = uuid::Uuid::new_v4().to_string();
        let server = "https://server.example";
        let message = PairIpcMessage {
            generation: generation.clone(),
            request_id: request_id.clone(),
            activation_url: format!("{server}/modules/host-monitoring/activate/{request_id}"),
            pairing_endpoint: format!(
                "{server}/api/modules/host-monitoring/agent/v2/pairing-requests"
            ),
        };
        validate_pair_ipc_message(&message, server).unwrap();

        let uppercase = PairIpcMessage {
            generation: generation.to_uppercase(),
            ..message
        };
        assert!(validate_pair_ipc_message(&uppercase, server).is_err());
    }

    #[test]
    fn bounded_line_reader_rejects_before_growing_past_the_limit() {
        let input = vec![b'x'; MAX_NDJSON_LINE_BYTES + 1];
        let mut reader = BufReader::with_capacity(1024, input.as_slice());
        let mut line = Vec::new();
        assert!(read_bounded_line(&mut reader, &mut line, MAX_NDJSON_LINE_BYTES).is_err());
        assert!(line.len() <= MAX_NDJSON_LINE_BYTES);
    }

    #[test]
    fn pairing_slot_stays_exclusive_for_the_full_worker_lifetime() {
        let state = Arc::new(LocalControlState {
            bootstrap_tokens: Mutex::new(Vec::new()),
            sessions: Mutex::new(Vec::new()),
            operations: Mutex::new(Vec::new()),
            active_pairings: AtomicUsize::new(0),
            active_service_operations: AtomicUsize::new(0),
            preferences_path: PathBuf::from("unused-test-preferences.json"),
        });
        let first = claim_pairing_slot(&state).unwrap();
        let (release, released) = std::sync::mpsc::channel();
        let worker = thread::spawn(move || {
            released.recv().unwrap();
            drop(first);
        });
        assert!(claim_pairing_slot(&state).is_err());
        release.send(()).unwrap();
        worker.join().unwrap();
        assert!(claim_pairing_slot(&state).is_ok());
    }

    #[test]
    fn connection_probe_distinguishes_unconfigured_and_healthy_server() {
        let unconfigured = probe_server_connection("");
        assert_eq!(unconfigured.status, "unconfigured");
        assert!(unconfigured.version.is_none());

        let healthy = probe_health_body(&format!(
            r#"{{"status":"ok","version":"{}","uptime_seconds":1}}"#,
            env!("CARGO_PKG_VERSION")
        ));
        assert_eq!(healthy.status, "online");
        assert_eq!(healthy.version.as_deref(), Some(env!("CARGO_PKG_VERSION")));
        assert!(healthy.latency_ms.is_some());
    }

    #[test]
    fn connection_probe_rejects_missing_or_mismatched_server_version() {
        let missing = probe_health_body(r#"{"status":"ok","uptime_seconds":1}"#);
        assert_eq!(missing.status, "offline");
        assert!(missing.version.is_none());
        assert_eq!(
            missing.message,
            "Server 未返回可用的 UnionC 健康状态（格式或版本信息无效）"
        );

        let mismatched = probe_health_body(
            r#"{"status":"ok","version":"incompatible-test-version","uptime_seconds":1}"#,
        );
        assert_eq!(mismatched.status, "offline");
        assert_eq!(
            mismatched.version.as_deref(),
            Some("incompatible-test-version")
        );
        assert!(mismatched.message.contains("版本不匹配"));
        assert!(mismatched.message.contains(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn connection_probe_rejects_non_unionc_success_response() {
        let invalid = probe_health_body("<html>not UnionC</html>");
        assert_eq!(invalid.status, "offline");
        assert!(invalid.message.contains("UnionC"));

        let current_body = format!(
            r#"{{"status":"ok","version":"{}","uptime_seconds":1}}"#,
            env!("CARGO_PKG_VERSION")
        );
        let wrong_status =
            probe_health_response("204 No Content", Some("application/json"), &current_body);
        assert_eq!(wrong_status.status, "offline");
        assert!(wrong_status.message.contains("204"));

        for content_type in [
            None,
            Some("text/plain"),
            Some("application/vnd.unionc+json"),
        ] {
            let wrong_type = probe_health_response("200 OK", content_type, &current_body);
            assert_eq!(wrong_type.status, "offline");
            assert!(wrong_type.message.contains("Content-Type"));
        }
    }
}
