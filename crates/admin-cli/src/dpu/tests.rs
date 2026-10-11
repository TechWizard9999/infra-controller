/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 * http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

// The intent of the tests.rs file is to test the integrity of the
// command, including things like basic structure parsing, enum
// translations, and any external input validators that are
// configured. Specific "categories" are:
//
// Command Structure - Baseline debug_assert() of the entire command.
// Argument Parsing  - Ensure required/optional arg combinations parse correctly.

use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use carbide_test_support::Outcome::*;
use carbide_test_support::scenarios;
use carbide_uuid::machine::{HostMachineId, MachineId, MachineIdSource, MachineType};
use clap::{CommandFactory, Parser};
use futures::stream;
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::{BodyExt, StreamBody};
use hyper::body::{Bytes, Frame};
use hyper::server::conn::http2;
use hyper::service::service_fn;
use hyper::{Response, header};
use hyper_util::rt::{TokioExecutor, TokioIo};
use prost::Message as _;
use rpc::admin_cli::OutputFormat;
use rpc::forge::dpu_reprovisioning_request::Mode;
use rpc::forge::{
    BuildInfo, DpuReprovisioningRequest, InsertMachineHealthReportRequest, Machine, MachineList,
    MachineStatus, MachinesByIdsRequest,
};
use rpc::forge_api_client::ForgeApiClient;
use rpc::forge_tls_client::{ApiConfig, ForgeClientConfig};
use tokio::net::TcpListener;

use super::*;
use crate::cfg::cli_options::SortField;
use crate::cfg::runtime::{RuntimeConfig, RuntimeContext};
use crate::rpc::ApiClient;
use crate::test_support::{parse_with_leaf_matches, raw_value};

// Define a basic/working MachineId for testing.
const TEST_MACHINE_ID: &str = "fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg";

// verify_cmd_structure runs a baseline clap debug_assert()
// to do basic command configuration checking and validation,
// ensuring things like unique argument definitions, group
// configurations, argument references, etc. Things that would
// otherwise be missed until runtime.
#[test]
fn verify_cmd_structure() {
    Cmd::command().debug_assert();
}

/////////////////////////////////////////////////////////////////////////////
// Argument Parsing
//
// This section contains tests specific to argument parsing,
// including testing required arguments, as well as optional
// flag-specific checking.

// versions parses with and without --updates-only; the parsed flag mirrors
// whether the switch was supplied.
#[test]
fn parse_versions() {
    scenarios!(
        run = |argv| {
            parse_with_leaf_matches::<Cmd>(argv, &["versions"])
                .map(|(cmd, matches)| {
                    assert!(matches!(cmd, Cmd::Versions(_)));
                    matches.get_flag("updates_only")
                })
                .map_err(drop)
        };
        "versions with no flags leaves updates_only off" {
            &["dpu", "versions"][..] => Yields(false),
        }

        "versions --updates-only sets the flag" {
            &["dpu", "versions", "--updates-only"][..] => Yields(true),
        }
    );
}

// reprovision routes to its three subcommands: list (no payload), set (an id
// plus the --update-firmware flag), and clear (an id). The closure yields the
// subcommand name, the machine id as a string (empty for list), and the
// update_firmware flag.
#[test]
fn parse_reprovision() {
    scenarios!(
        run = |argv| {
            let subcommand = argv[2];
            parse_with_leaf_matches::<Cmd>(argv, &["reprovision", subcommand])
                .map(|(cmd, _)| {
                    match cmd {
                        Cmd::Reprovision(reprovision::Args::List) => {
                            ("list", String::new(), false)
                        }
                        Cmd::Reprovision(reprovision::Args::Set(args)) => {
                            let request = DpuReprovisioningRequest::from(&args);
                            assert_eq!(request.mode, Mode::Set as i32);
                            (
                                "set",
                                request.dpu_id.expect("machine ID is required").to_string(),
                                request.update_firmware,
                            )
                        }
                        Cmd::Reprovision(reprovision::Args::Clear(args)) => {
                            let request = DpuReprovisioningRequest::from(&args);
                            assert_eq!(request.mode, Mode::Clear as i32);
                            (
                                "clear",
                                request.dpu_id.expect("machine ID is required").to_string(),
                                request.update_firmware,
                            )
                        }
                        _ => panic!("expected Reprovision variant"),
                    }
                })
                .map_err(drop)
        };
        "reprovision list routes to List" {
            &["dpu", "reprovision", "list"][..] => Yields(("list", String::new(), false)),
        }

        "reprovision set carries the machine id, firmware off" {
            &["dpu", "reprovision", "set", "--id", TEST_MACHINE_ID][..] => Yields(("set", TEST_MACHINE_ID.to_string(), false)),
        }

        "reprovision clear carries the machine id" {
            &["dpu", "reprovision", "clear", "--id", TEST_MACHINE_ID][..] => Yields(("clear", TEST_MACHINE_ID.to_string(), false)),
        }
    );
}

// agent-upgrade-policy parses with no --set (get, leaving set unset) and with
// --set up-only (selecting the UpOnly choice). The closure yields the policy
// name the parsed --set resolves to ("<get>" when unset).
#[test]
fn parse_agent_upgrade_policy() {
    scenarios!(
        run = |argv| {
            parse_with_leaf_matches::<Cmd>(argv, &["agent-upgrade-policy"])
                .map(|(cmd, matches)| {
                    assert!(matches!(cmd, Cmd::AgentUpgradePolicy(_)));
                    raw_value(&matches, "set").unwrap_or_else(|| "<get>".to_string())
                })
                .map_err(drop)
        };
        "no --set is a get and leaves the policy unset" {
            &["dpu", "agent-upgrade-policy"][..] => Yields("<get>".to_string()),
        }

        "--set up-only selects the UpOnly policy" {
            &["dpu", "agent-upgrade-policy", "--set", "up-only"][..] => Yields("up-only".to_string()),
        }
    );
}

fn reprovision_grpc_response(
    message: impl prost::Message,
) -> Response<UnsyncBoxBody<Bytes, Infallible>> {
    let mut data = vec![0];
    data.extend_from_slice(&u32::try_from(message.encoded_len()).unwrap().to_be_bytes());
    message.encode(&mut data).unwrap();
    let mut trailers = hyper::HeaderMap::new();
    trailers.insert(
        header::HeaderName::from_static("grpc-status"),
        header::HeaderValue::from_static("0"),
    );
    let body = StreamBody::new(stream::iter([
        Ok::<_, Infallible>(Frame::data(Bytes::from(data))),
        Ok(Frame::trailers(trailers)),
    ]))
    .boxed_unsync();
    Response::builder()
        .header(header::CONTENT_TYPE, "application/grpc+tonic")
        .body(body)
        .unwrap()
}

/// Exercise the health-report RPC before Set, including the DPU-to-host lookup.
#[tokio::test]
async fn reprovision_update_message_targets_stable_and_predicted_hosts() {
    let stable_id: MachineId = TEST_MACHINE_ID.parse().unwrap();
    let predicted_id = MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [0xAE; 32],
        MachineType::PredictedHost,
    );
    let dpu_id = MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [0xAF; 32],
        MachineType::Dpu,
    );
    for (target_id, host_id, expected_reads) in [
        (stable_id, stable_id, 1),
        (predicted_id, predicted_id, 1),
        (dpu_id, predicted_id, 2),
    ] {
        let reads = Arc::new(AtomicUsize::new(0));
        let reports = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(AtomicUsize::new(0));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let client_config = ForgeClientConfig {
            request_timeout: Some(Duration::from_secs(5)),
            ..Default::default()
        };
        let ctx = RuntimeContext {
            api_client: ApiClient(ForgeApiClient::new(&ApiConfig::new(
                &format!("http://{address}"),
                &client_config,
            ))),
            config: RuntimeConfig {
                format: OutputFormat::AsciiTable,
                request_timeout: client_config.request_timeout,
                page_size: 25,
                extended: false,
                cloud_unsafe_op: None,
                sort_by: SortField::PrimaryId,
            },
            output_file: Box::new(tokio::io::sink()),
        };
        let recorded_reads = reads.clone();
        let recorded_reports = reports.clone();
        let recorded_requests = requests.clone();
        let server = tokio::spawn(async move {
            let (connection, _) = listener.accept().await.unwrap();
            http2::Builder::new(TokioExecutor::new())
                .serve_connection(
                    TokioIo::new(connection),
                    service_fn(move |request: hyper::Request<hyper::body::Incoming>| {
                        let reads = recorded_reads.clone();
                        let reports = recorded_reports.clone();
                        let requests = recorded_requests.clone();
                        async move {
                            let path = request.uri().path().to_string();
                            let body = request.into_body().collect().await.unwrap().to_bytes();
                            let payload = body.get(5..).unwrap();
                            let response = match path.as_str() {
                                "/forge.Forge/Version" => {
                                    reprovision_grpc_response(BuildInfo::default())
                                }
                                "/forge.Forge/FindMachinesByIds" => {
                                    let request = MachinesByIdsRequest::decode(payload).unwrap();
                                    let index = reads.fetch_add(1, Ordering::SeqCst);
                                    let requested_id = if index == 0 { target_id } else { host_id };
                                    assert_eq!(request.machine_ids, vec![requested_id]);
                                    reprovision_grpc_response(MachineList {
                                        machines: vec![Machine {
                                            id: Some(requested_id),
                                            status: Some(MachineStatus {
                                                associated_host_machine_id: if requested_id
                                                    == dpu_id
                                                {
                                                    Some(HostMachineId::try_from(host_id).unwrap())
                                                } else {
                                                    None
                                                },
                                                ..Default::default()
                                            }),
                                            ..Default::default()
                                        }],
                                    })
                                }
                                "/forge.Forge/InsertMachineHealthReport" => {
                                    let request =
                                        InsertMachineHealthReportRequest::decode(payload).unwrap();
                                    assert_eq!(request.machine_id, Some(host_id));
                                    let report =
                                        request.health_report_entry.unwrap().report.unwrap();
                                    assert_eq!(report.source, "host-update");
                                    assert_eq!(report.alerts.len(), 1);
                                    reports.fetch_add(1, Ordering::SeqCst);
                                    reprovision_grpc_response(())
                                }
                                "/forge.Forge/TriggerDpuReprovisioning" => {
                                    let request =
                                        DpuReprovisioningRequest::decode(payload).unwrap();
                                    assert_eq!(request.machine_id, Some(target_id));
                                    assert_eq!(request.mode, Mode::Set as i32);
                                    assert_eq!(reports.load(Ordering::SeqCst), 1);
                                    requests.fetch_add(1, Ordering::SeqCst);
                                    reprovision_grpc_response(())
                                }
                                _ => panic!("unexpected RPC: {path}"),
                            };
                            Ok::<_, Infallible>(response)
                        }
                    }),
                )
                .await
                .unwrap();
        });
        let command = Cmd::try_parse_from([
            "dpu",
            "reprovision",
            "set",
            "--id",
            &target_id.to_string(),
            "--update-message",
            "recover failed ingestion",
        ])
        .unwrap();
        let result = tokio::time::timeout(Duration::from_secs(5), command.dispatch(ctx)).await;
        server.abort();
        if let Err(error) = server.await {
            assert!(error.is_cancelled(), "mock server failed: {error}");
        }
        result.unwrap().unwrap();
        assert_eq!(reads.load(Ordering::SeqCst), expected_reads);
        assert_eq!(reports.load(Ordering::SeqCst), 1);
        assert_eq!(requests.load(Ordering::SeqCst), 1);
    }
}
