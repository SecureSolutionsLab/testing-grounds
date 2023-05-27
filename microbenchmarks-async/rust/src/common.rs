#![allow(dead_code)]

use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::iter;
use std::sync::Arc;
use std::time::Duration;

use intmap::IntMap;
use konst::primitive::{parse_u128, parse_usize};
use konst::unwrap_ctx;
use regex::Regex;
use rustls::{Certificate, ClientConfig, PrivateKey, RootCertStore, ServerConfig};
use rustls::server::AllowAnyAuthenticatedClient;
use rustls_pemfile::{Item, read_one};
use febft_pbft_consensus::bft::message::ObserveEventKind;
use febft_client::client;
use febft_client::client::Client;
use febft_client::client::unordered_client::UnorderedClientMode;
use febft_common::crypto::signature::{KeyPair, PublicKey};
use febft_common::ordering::{Orderable, SeqNo};
use febft_common::error::*;
use febft_common::node_id::NodeId;
use febft_common::threadpool;
use febft_communication::config::{ClientPoolConfig, MioConfig, NodeConfig, PKConfig, TcpConfig, TlsConfig};
use febft_communication::mio_tcp::MIOTcpNode;
use febft_communication::tcp_ip_simplex::TCPSimplexNode;
use febft_communication::tcpip::{PeerAddr, TcpNode};
use febft_messages::serialize::{ClientServiceMsg, ServiceMsg};
use febft_metrics::benchmarks::CommStats;
use febft_metrics::InfluxDBArgs;
use febft_pbft_consensus::bft::message::serialize::PBFTConsensus;
use febft_pbft_consensus::bft::{PBFTOrderProtocol};
use febft_pbft_consensus::bft::config::{PBFTConfig, ProposerConfig};
use febft_pbft_consensus::bft::sync::view::ViewInfo;
use febft_replica::config::ReplicaConfig;
use febft_replica::server::{Replica};
use febft_state_transfer::CollabStateTransfer;
use febft_state_transfer::config::StateTransferConfig;
use febft_state_transfer::message::serialize::CSTMsg;

use crate::exec::Microbenchmark;
use crate::serialize::MicrobenchmarkData;

#[macro_export]
macro_rules! addr {
    ($h:expr => $a:expr) => {{
        let addr: ::std::net::SocketAddr = $a.parse().unwrap();
        (addr, String::from($h))
    }}
}

/*
pub struct ObserverCall;

impl ObserverCallback for ObserverCall {
    fn handle_event(&self, event: ObserveEventKind, n: usize) {
        match event {
            ObserveEventKind::CheckpointStart(start_cp) => {
                println!("Received checkpoint start with seq {:?}", start_cp)
            }
            ObserveEventKind::CheckpointEnd(end_cp) => {
                println!("Received checkpoint end with seq {:?}", end_cp)
            }
            ObserveEventKind::Consensus(consensus_instance) => {
                println!("Received consensus phase with seq {:?}", consensus_instance)
            }
            ObserveEventKind::NormalPhase((view, seq)) => {
                println!("Received normal phase message with seq {:?} for view {:?} with the current leader {:?}", seq, view.sequence_number(), view.leader());
            }
            ObserveEventKind::ViewChangePhase => {
                println!("Received view change phase message")
            }
            ObserveEventKind::CollabStateTransfer => {
                println!("Received collab state transfer message")
            }
            ObserveEventKind::Prepare(seq) => {
                println!("Received prepare stage with seq {:?}", seq)
            }
            ObserveEventKind::Commit(seq) => {
                println!("Received commit stage with seq {:?}", seq)
            }
            ObserveEventKind::Ready(_) => {}
            ObserveEventKind::Executed(_) => {}
        }
    }
}

 */

pub struct ConfigEntry {
    pub portno: u16,
    pub rep_portno: Option<u16>,
    pub id: u32,
    pub hostname: String,
    pub ipaddr: String,
}

pub fn parse_config(path: &str) -> Option<Vec<ConfigEntry>> {
    let re = Regex::new("([^ ]+)").ok()?;

    let file = File::open(path).ok()?;
    let mut file = BufReader::new(file);

    let mut buf = String::new();
    let mut config = Vec::new();

    loop {
        match file.read_line(&mut buf) {
            Ok(0) => {
                println!("Finalizing reading the config file {} with size {}", path, config.len());

                break;
            }
            Err(err) => {
                println!("Finalizing reading the config file {} with size {} with err {:?}", path, config.len(), err);

                break;
            }
            _ => {
                match parse_entry(&re, &buf) {
                    Some(entry) => config.push(entry),
                    None => {
                        println!("Failed to parse entry: {}", buf);
                    }
                }
                buf.clear();
            }
        }
    }

    Some(config)
}

fn parse_entry(re: &Regex, line: &str) -> Option<ConfigEntry> {
    let line = line.trim();

    if line.chars().next() == Some('#') {
        return None;
    }

    let matches: Vec<_> = re
        .find_iter(line)
        .collect();

    if matches.len() < 4 {
        return None;
    }

    let id: u32 = matches[0].as_str().parse().ok()?;
    let hostname: String = matches[1].as_str().to_string();
    let ipaddr: String = matches[2].as_str().to_string();
    let portno: u16 = matches[3].as_str().trim_end().parse().ok()?;

    let rep_portno: Option<u16> =
        if matches.len() >= 5 {
            Some(matches[4].as_str().trim_end().parse().ok()?)
        } else { None };

    Some(ConfigEntry { id, rep_portno, hostname, ipaddr, portno })
}

/// Get the configuration for influx DB
pub fn influx_db_config(id: NodeId) -> InfluxDBArgs {
    let ip = std::env::var("INFLUX_IP").expect("INFLUX_IP not set");
    let db_name = std::env::var("INFLUX_DB_NAME").expect("INFLUX_DB_NAME not set");
    let user = std::env::var("INFLUX_USER").expect("INFLUX_USER not set");
    let password = std::env::var("INFLUX_PASSWORD").expect("INFLUX_PASSWORD not set");

    let extra = std::env::var("INFLUX_EXTRA_TEST").unwrap_or("".to_string());

    let extra = Some(format!("test-{}", extra));

    InfluxDBArgs {
        ip,
        db_name,
        user,
        password,
        node_id: id,
        extra,
    }
}

async fn node_config(
    n: usize,
    id: NodeId,
    sk: KeyPair,
    addrs: IntMap<PeerAddr>,
    pk: IntMap<PublicKey>,
    comm_stats: Option<Arc<CommStats>>,
) -> MioConfig {
    let first_cli = NodeId::from(1000u32);

    let mio_worker_count = std::env::var("MIO_WORKER_COUNT").unwrap_or("1".to_string()).parse::<usize>().unwrap();

    // read TLS configs concurrently
    let (client_config, server_config,
        client_config_replica, server_config_replica,
        batch_size, batch_timeout, batch_sleep, clients_per_pool) = {
        let cli = get_client_config(id);
        let srv = get_tls_sync_server_config(id);
        let cli_rustls = get_client_config_replica(id);
        let srv_rustls = get_server_config_replica(id);
        let batch_size = get_batch_size();
        let batch_timeout = get_batch_timeout();
        let batch_sleep = get_batch_sleep();
        let clients_per_pool = get_clients_per_pool();
        futures::join!(cli, srv, cli_rustls, srv_rustls, batch_size,
            batch_timeout, batch_sleep, clients_per_pool)
    };

    let tcp = TcpConfig {
        addrs,
        network_config: TlsConfig {
            async_client_config: client_config,
            async_server_config: server_config,
            sync_client_config: client_config_replica,
            sync_server_config: server_config_replica,
        },
        replica_concurrent_connections: 4,
        client_concurrent_connections: 2,
    };

    let cp = ClientPoolConfig {
        batch_size,
        clients_per_pool,
        batch_timeout_micros: batch_timeout as u64,
        batch_sleep_micros: batch_sleep as u64,
    };

    let pk_config = PKConfig {
        sk,
        pk,
    };

    let node_config = NodeConfig {
        id,
        first_cli,
        tcp_config: tcp,
        client_pool_config: cp,
        pk_crypto_config: pk_config,
    };

    let mio_cfg = MioConfig {
        node_config,
        worker_count: mio_worker_count,
    };

    mio_cfg
}

/// Set up the data handles so we initialize the networking layer
pub type OrderProtocolMessage = PBFTConsensus<MicrobenchmarkData>;
pub type StateTransferMessage = CSTMsg<MicrobenchmarkData, OrderProtocolMessage, OrderProtocolMessage>;

/// Set up the networking layer with the data handles we have
pub type ReplicaNetworking = MIOTcpNode<ServiceMsg<MicrobenchmarkData, OrderProtocolMessage, StateTransferMessage>>;
pub type ClientNetworking = MIOTcpNode<ClientServiceMsg<MicrobenchmarkData>>;

/// Set up the protocols with the types that have been built up to here
pub type OrderProtocol = PBFTOrderProtocol<MicrobenchmarkData, StateTransferMessage, ReplicaNetworking>;
pub type StateTransferProtocol = CollabStateTransfer<MicrobenchmarkData, OrderProtocol, ReplicaNetworking>;

pub async fn setup_client(
    n: usize,
    id: NodeId,
    sk: KeyPair,
    addrs: IntMap<PeerAddr>,
    pk: IntMap<PublicKey>,
    comm_stats: Option<Arc<CommStats>>,
) -> Result<Client<MicrobenchmarkData, ClientNetworking>> {
    let node = node_config(n, id, sk, addrs, pk, comm_stats).await;

    let conf = client::ClientConfig {
        n,
        f: 1,
        unordered_rq_mode: UnorderedClientMode::BFT,
        node,
    };

    Client::bootstrap(conf).await
}

pub async fn setup_replica(
    n: usize,
    id: NodeId,
    sk: KeyPair,
    addrs: IntMap<PeerAddr>,
    pk: IntMap<PublicKey>,
    comm_stats: Option<Arc<CommStats>>,
) -> Result<Replica<Microbenchmark, OrderProtocol, StateTransferProtocol, ReplicaNetworking>> {
    let node_id = id.clone();
    let db_path = format!("PERSISTENT_DB_{:?}", id);

    let (node, global_batch_size, global_batch_timeout) = {
        let n = node_config(n, id, sk, addrs, pk, comm_stats);
        let b = get_global_batch_size();
        let timeout = get_global_batch_timeout();
        futures::join!(n, b, timeout)
    };

    let max_batch_size = get_max_batch_size();

    let proposer_config = ProposerConfig {
        target_batch_size: global_batch_size as u64,
        max_batch_size: global_batch_size as u64 * 2,
        batch_timeout: global_batch_timeout as u64,
    };

    let view = ViewInfo::new(SeqNo::ZERO, n, 1)?;

    let timeout_duration = Duration::from_secs(3);

    let op_config = PBFTConfig::new(node_id, None,
                                    view, timeout_duration.clone(),
                                    130, db_path, proposer_config);

    let st_config = StateTransferConfig {
        timeout_duration,
    };

    let conf = ReplicaConfig::<Microbenchmark, OrderProtocol, StateTransferProtocol, ReplicaNetworking> {
        node,
        view: SeqNo::ZERO,
        next_consensus_seq: SeqNo::ZERO,
        service: Microbenchmark::new(node_id),
        id,
        n,
        f: 1,
        op_config,
        st_config,
    };

    Replica::bootstrap(conf).await
}

async fn get_batch_size() -> usize {
    let (tx, rx) = oneshot::channel();
    threadpool::execute(move || {
        let mut buf = String::new();
        let mut f = open_file("./config/batch.config");
        f.read_to_string(&mut buf).unwrap();
        tx.send(buf.trim().parse().unwrap()).unwrap();
    });
    rx.await.unwrap()
}

async fn get_global_batch_size() -> usize {
    let (tx, rx) = oneshot::channel();
    threadpool::execute(move || {
        let res = parse_usize(&*std::env::var("GLOBAL_BATCH_SIZE")
            .expect("Failed to find required env var GLOBAL_BATCH_SIZE"));

        tx.send(unwrap_ctx!(res)).expect("Failed to send");
    });
    rx.await.unwrap()
}

async fn get_global_batch_timeout() -> u128 {
    let (tx, rx) = oneshot::channel();
    threadpool::execute(move || {
        let res = parse_u128(&*std::env::var("GLOBAL_BATCH_SLEEP_MICROS")
            .expect("Failed to find required env var GLOBAL_BATCH_SLEEP_MICROS"));

        tx.send(unwrap_ctx!(res)).expect("Failed to send");
    });
    rx.await.unwrap()
}

fn get_max_batch_size() -> usize {
    let res = parse_usize(&*std::env::var("MAX_BATCH_SIZE")
        .expect("Failed to find required env var MAX_BATCH_SIZE"));

    unwrap_ctx!(res)
}

async fn get_batch_timeout() -> u128 {
    let (tx, rx) = oneshot::channel();
    threadpool::execute(move || {
        let res = parse_u128(&*std::env::var("BATCH_TIMEOUT_MICROS")
            .expect("Failed to find required env var BATCH_TIMEOUT_MICROS"));

        tx.send(unwrap_ctx!(res)).expect("Failed to send");
    });
    rx.await.unwrap()
}

async fn get_batch_sleep() -> u128 {
    let (tx, rx) = oneshot::channel();
    threadpool::execute(move || {
        let res = parse_u128(&*std::env::var("BATCH_SLEEP_MICROS")
            .expect("Failed to find required env var BATCH_SLEEP_MICROS"));

        tx.send(unwrap_ctx!(res)).expect("Failed to send");
    });
    rx.await.unwrap()
}

async fn get_clients_per_pool() -> usize {
    let (tx, rx) = oneshot::channel();
    threadpool::execute(move || {
        let res = parse_usize(&*std::env::var("CLIENTS_PER_POOL")
            .expect("Failed to find required env var CLIENTS_PER_POOL"));

        tx.send(unwrap_ctx!(res)).expect("Failed to send");
    });
    rx.await.unwrap()
}


pub fn get_concurrent_rqs() -> usize {
    let res = parse_usize(&*std::env::var("CONCURRENT_RQS")
        .expect("Failed to find required env var CONCURRENT_RQS"));

    unwrap_ctx!(res)
}


fn read_certificates_from_file(mut file: &mut BufReader<File>) -> Vec<Certificate> {
    let mut certs = Vec::new();

    for item in iter::from_fn(|| read_one(&mut file).transpose()) {
        match item.unwrap() {
            Item::X509Certificate(cert) => {
                certs.push(Certificate(cert));
            }
            Item::RSAKey(_) => {
                panic!("Key given in place of a certificate")
            }
            Item::PKCS8Key(_) => {
                panic!("Key given in place of a certificate")
            }
            Item::ECKey(_) => {
                panic!("Key given in place of a certificate")
            }
            _ => {
                panic!("Key given in place of a certificate")
            }
        }
    }

    certs
}

#[inline]
fn read_private_keys_from_file(mut file: BufReader<File>) -> Vec<PrivateKey> {
    let mut certs = Vec::new();

    for item in iter::from_fn(|| read_one(&mut file).transpose()) {
        match item.unwrap() {
            Item::RSAKey(rsa) => {
                certs.push(PrivateKey(rsa))
            }
            Item::PKCS8Key(rsa) => {
                certs.push(PrivateKey(rsa))
            }
            Item::ECKey(rsa) => {
                certs.push(PrivateKey(rsa))
            }
            _ => {
                panic!("Key given in place of a certificate")
            }
        }
    }

    certs
}

#[inline]
fn read_private_key_from_file(mut file: BufReader<File>) -> PrivateKey {
    read_private_keys_from_file(file).pop().unwrap()
}

async fn get_tls_sync_server_config(id: NodeId) -> ServerConfig {
    let (tx, rx) = oneshot::channel();
    threadpool::execute(move || {
        let id = usize::from(id);
        let mut root_store = RootCertStore::empty();

        // read ca file
        let cert = {
            let mut file = open_file("./ca-root/crt");

            let certs = read_certificates_from_file(&mut file);

            root_store.add(&certs[0]).expect("Failed to put root store");

            certs
        };

        // configure our cert chain and secret key
        let sk = {
            let mut file = if id < 1000 {
                open_file(&format!("./ca-root/srv{}/key", id))
            } else {
                open_file(&format!("./ca-root/cli{}/key", id))
            };

            read_private_key_from_file(file)
        };

        let chain = {
            let mut file = if id < 1000 {
                open_file(&format!("./ca-root/srv{}/crt", id))
            } else {
                open_file(&format!("./ca-root/cli{}/crt", id))
            };

            let mut certs = read_certificates_from_file(&mut file);

            certs.extend(cert);
            certs
        };

        // create server conf
        let auth = AllowAnyAuthenticatedClient::new(root_store);
        let cfg = ServerConfig::builder()
            .with_safe_default_cipher_suites()
            .with_safe_default_kx_groups()
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_client_cert_verifier(Arc::new(auth))
            .with_single_cert(chain, sk)
            .expect("Failed to make cfg");

        tx.send(cfg).unwrap();
    });
    rx.await.unwrap()
}

async fn get_server_config_replica(id: NodeId) -> rustls::ServerConfig {
    let (tx, rx) = oneshot::channel();

    threadpool::execute(move || {
        let id = usize::from(id);
        let mut root_store = RootCertStore::empty();

        // read ca file
        let certs = {
            let mut file = open_file("./ca-root/crt");

            read_certificates_from_file(&mut file)
        };

        root_store.add(&certs[0]).unwrap();

        // configure our cert chain and secret key
        let sk = {
            let mut file = if id < 1000 {
                open_file(&format!("./ca-root/srv{}/key", id))
            } else {
                open_file(&format!("./ca-root/cli{}/key", id))
            };

            read_private_key_from_file(file)
        };
        let chain = {
            let mut file = if id < 1000 {
                open_file(&format!("./ca-root/srv{}/crt", id))
            } else {
                open_file(&format!("./ca-root/cli{}/crt", id))
            };

            let mut c = read_certificates_from_file(&mut file);

            c.extend(certs);

            c
        };

        // create server conf
        let auth = AllowAnyAuthenticatedClient::new(root_store);

        let cfg = ServerConfig::builder()
            .with_safe_default_cipher_suites()
            .with_safe_default_kx_groups()
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_client_cert_verifier(Arc::new(auth))
            .with_single_cert(chain, sk)
            .expect("Failed to make cfg");

        tx.send(cfg).unwrap();
    });
    rx.await.unwrap()
}

async fn get_client_config(id: NodeId) -> ClientConfig {
    let (tx, rx) = oneshot::channel();
    threadpool::execute(move || {
        let id = usize::from(id);

        let mut root_store = RootCertStore::empty();

        // configure ca file
        let certs = {
            let mut file = open_file("./ca-root/crt");
            read_certificates_from_file(&mut file)
        };

        root_store.add(&certs[0]).unwrap();

        // configure our cert chain and secret key
        let sk = {
            let mut file = if id < 1000 {
                open_file(&format!("./ca-root/srv{}/key", id))
            } else {
                open_file(&format!("./ca-root/cli{}/key", id))
            };

            read_private_key_from_file(file)
        };

        let chain = {
            let mut file = if id < 1000 {
                open_file(&format!("./ca-root/srv{}/crt", id))
            } else {
                open_file(&format!("./ca-root/cli{}/crt", id))
            };
            let mut c = read_certificates_from_file(&mut file);

            c.extend(certs);
            c
        };

        let cfg = ClientConfig::builder()
            .with_safe_default_cipher_suites()
            .with_safe_default_kx_groups()
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(root_store)
            .with_single_cert(chain, sk)
            .expect("bad cert/key");

        tx.send(cfg).unwrap();
    });
    rx.await.unwrap()
}

async fn get_client_config_replica(id: NodeId) -> rustls::ClientConfig {
    let (tx, rx) = oneshot::channel();

    threadpool::execute(move || {
        let id = usize::from(id);

        let mut root_store = RootCertStore::empty();

        // configure ca file
        let certs = {
            let mut file = open_file("./ca-root/crt");
            read_certificates_from_file(&mut file)
        };

        root_store.add(&certs[0]).unwrap();

        // configure our cert chain and secret key
        let sk = {
            let mut file = if id < 1000 {
                open_file(&format!("./ca-root/srv{}/key", id))
            } else {
                open_file(&format!("./ca-root/cli{}/key", id))
            };

            read_private_key_from_file(file)
        };

        let chain = {
            let mut file = if id < 1000 {
                open_file(&format!("./ca-root/srv{}/crt", id))
            } else {
                open_file(&format!("./ca-root/cli{}/crt", id))
            };
            let mut c = read_certificates_from_file(&mut file);

            c.extend(certs);
            c
        };

        let cfg = ClientConfig::builder()
            .with_safe_default_cipher_suites()
            .with_safe_default_kx_groups()
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(root_store)
            .with_single_cert(chain, sk)
            .expect("bad cert/key");

        tx.send(cfg).unwrap();
    });

    rx.await.unwrap()
}


fn open_file(path: &str) -> BufReader<File> {
    let file = File::open(path).expect(path);
    BufReader::new(file)
}