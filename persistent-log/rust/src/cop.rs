use crate::common::*;
use crate::serialize::MicrobenchmarkData;

use std::fs::File;
use std::io::Write;
use std::sync::Arc;
use std::time::{Duration};

use intmap::IntMap;
use chrono::offset::Utc;
use konst::primitive::parse_u32;
use rand_core::{OsRng, RngCore};
use nolock::queues::mpsc::jiffy::{
    async_queue,
    AsyncSender,
};

use febft::bft::communication::{channel, PeerAddr};
use febft::bft::core::client::Client;
use febft::bft::communication::NodeId;
use febft::bft::async_runtime as rt;
use febft::bft::{
    init,
    InitConfig,
};
use semaphores::RawSemaphore;
use febft::bft::crypto::signature::{
    KeyPair,
    PublicKey,
};
use febft::bft::benchmarks::{ CommStats};

pub fn main() {
    let is_client = std::env::var("CLIENT")
        .map(|x| x == "1")
        .unwrap_or(false);

    let threadpool_threads = parse_u32(std::env::var("THREADPOOL_THREADS")
        .unwrap_or(String::from("2")).as_str()).unwrap();

    let conf = InitConfig {
        //If we are the client, we want to have many threads to send stuff to replicas
        threadpool_threads: threadpool_threads as usize,
        async_threads: if is_client { 2 } else { 2 },
        //If we are the client, we don't want any threads to send to other clients as that will never happen
    };

    let _guard = unsafe { init(conf).unwrap() };
    if !is_client {
        let replica_id: u32 = std::env::var("ID")
            .iter()
            .flat_map(|id| id.parse())
            .next()
            .unwrap();

        main_(NodeId::from(replica_id));
    } else {
        rt::block_on(client_async_main());
    }
}

fn main_(id: NodeId) {
    let mut replica = {
        println!("Started working on the replica");

        //TODO: don't have this hardcoded?
        let first_cli = NodeId::from(1000u32);

        let clients_config = parse_config("./config/clients.config").unwrap();
        let replicas_config = parse_config("./config/replicas.config").unwrap();

        println!("Finished reading replica config.");

        let mut secret_keys: IntMap<KeyPair> = sk_stream()
            .take(clients_config.len())
            .enumerate()
            .map(|(id, sk)| (1000 + id as u64, sk))
            .chain(sk_stream()
                .take(replicas_config.len())
                .enumerate()
                .map(|(id, sk)| (id as u64, sk)))
            .collect();

        let public_keys: IntMap<PublicKey> = secret_keys
            .iter()
            .map(|(id, sk)| (*id, sk.public_key().into()))
            .collect();

        println!("Finished reading keys.");

        let addrs = {
            let mut addrs = IntMap::new();
            for replica in &replicas_config {
                let id = NodeId::from(replica.id);
                let addr = format!("{}:{}", replica.ipaddr, replica.portno);
                let replica_addr = format!("{}:{}", replica.ipaddr, replica.rep_portno.unwrap());

                let replica_p_addr = PeerAddr::new_replica(crate::addr!(&replica.hostname => addr),
                                                           crate::addr!(&replica.hostname => replica_addr));

                addrs.insert(id.into(), replica_p_addr);
            }

            for other in &clients_config {
                let id = NodeId::from(other.id);
                let addr = format!("{}:{}", other.ipaddr, other.portno);

                let client_addr = PeerAddr::new(crate::addr!(&other.hostname => addr));

                addrs.insert(id.into(), client_addr);
            }

            addrs
        };

        let comm_stats = Arc::new(CommStats::new(id,
                                                 first_cli,
                                                 100000));

        let sk = secret_keys.remove(id.into()).unwrap();

        let fut = setup_replica(
            replicas_config.len(),
            id,
            sk,
            addrs,
            public_keys.clone(),
            Some(comm_stats)
        );

        println!("Bootstrapping replica #{}", u32::from(id));
        let replica = rt::block_on(fut).unwrap();
        println!("Running replica #{}", u32::from(id));

        //Here we want to launch a statistics thread for each replica since they are on different machines
        //crate::os_statistics::start_statistics_thread(id);

        replica
    };

    // run forever
    replica.run().unwrap();
}

async fn client_async_main() {

    let clients_config = parse_config("./config/clients.config").unwrap();
    let replicas_config = parse_config("./config/replicas.config").unwrap();

    let mut first_cli = 1000u32;

    let mut local_first_cli = u32::MAX;

    for client in &clients_config {
        if client.id < local_first_cli {
            local_first_cli = client.id;
        }
    }

    //Start the OS resource monitoring thread
    crate::os_statistics::start_statistics_thread(NodeId(local_first_cli));

    let mut secret_keys: IntMap<KeyPair> = sk_stream()
        .take(clients_config.len())
        .enumerate()
        .map(|(id, sk)| (local_first_cli as u64 + id as u64, sk))
        .chain(sk_stream()
            .take(replicas_config.len())
            .enumerate()
            .map(|(id, sk)| (id as u64, sk)))
        .collect();
    let public_keys: IntMap<PublicKey> = secret_keys
        .iter()
        .map(|(id, sk)| (*id, sk.public_key().into()))
        .collect();

    let (tx, mut rx) = channel::new_bounded_async(8);

    let comm_stats = Arc::new(CommStats::new(NodeId::from(local_first_cli),
                                             NodeId::from(first_cli),
                                             100000));

    for client in &clients_config {
        let id = NodeId::from(client.id);

        let addrs = {
            let mut addrs = IntMap::new();
            for replica in &replicas_config {
                let id = NodeId::from(replica.id);
                let addr = format!("{}:{}", replica.ipaddr, replica.portno);
                let replica_addr = format!("{}:{}", replica.ipaddr, replica.rep_portno.unwrap());

                let replica_p_addr = PeerAddr::new_replica(crate::addr!(&replica.hostname => addr),
                                                           crate::addr!(&replica.hostname => replica_addr));

                addrs.insert(id.into(), replica_p_addr);
            }

            for other in &clients_config {
                let id = NodeId::from(other.id);
                let addr = format!("{}:{}", other.ipaddr, other.portno);

                let client_addr = PeerAddr::new(crate::addr!(&other.hostname => addr));

                addrs.insert(id.into(), client_addr);
            }

            addrs
        };

        let sk = secret_keys.remove(id.into()).unwrap();
        let fut = setup_client(
            replicas_config.len(),
            id,
            sk,
            addrs,
            public_keys.clone(),
            Some(comm_stats.clone())
        );

        let mut tx = tx.clone();
        rt::spawn(async move {
            //We can have this async initializer no problem, just don't want it to be used to actually send
            //The requests/control the clients
            println!("Bootstrapping client #{}", u32::from(id));
            let client = fut.await.unwrap();
            println!("Done bootstrapping client #{}", u32::from(id));
            tx.send(client).await.unwrap();
        });
    }

    drop((secret_keys, public_keys, replicas_config));

    let (mut queue, queue_tx) = async_queue();
    let queue_tx = Arc::new(queue_tx);

    let mut clients = Vec::with_capacity(clients_config.len());

    for _i in 0..clients_config.len() {
        clients.push(rx.recv().await.unwrap());
    }

    let mut handles = Vec::with_capacity(clients_config.len());

    for client in clients {
        let queue_tx = Arc::clone(&queue_tx);
        let id = client.id();

        let h = std::thread::Builder::new()
            .name(format!("Client {:?}", client.id()))
            .spawn(move || { run_client(client, queue_tx) })
            .expect(format!("Failed to start thread for client {:?} ", &id.id()).as_str());

        handles.push(h);

        // Delay::new(Duration::from_millis(5)).await;
    }

    drop(clients_config);

    for h in handles {
        let _ = h.join();
    }

    let mut file = File::create("./latencies.out").unwrap();

    while let Ok(line) = queue.try_dequeue() {
        file.write_all(line.as_ref()).unwrap();
    }

    file.flush().unwrap();
}

fn sk_stream() -> impl Iterator<Item=KeyPair> {
    std::iter::repeat_with(|| {
        // only valid for ed25519!
        let buf = [0; 32];
        KeyPair::from_bytes(&buf[..]).unwrap()
    })
}

fn run_client(mut client: Client<MicrobenchmarkData>, q: Arc<AsyncSender<String>>) {
    let concurrent_rqs: usize = get_concurrent_rqs();

    let id = u32::from(client.id());

    println!("Warm up...");

    let request = Arc::new({
        let mut r = vec![0; MicrobenchmarkData::REQUEST_SIZE];
        OsRng.fill_bytes(&mut r);
        r
    });

    let semaphore = Arc::new(RawSemaphore::new(concurrent_rqs));

    let iterator = 0..(MicrobenchmarkData::OPS_NUMBER / 2);

    let mut ramp_up = 1000;

    for req in iterator {

        //Only allow concurrent_rqs per client at the network
        semaphore.acquire();

        if MicrobenchmarkData::VERBOSE {
            println!("{:?} // Sending req {}...", client.id(), req);
        }

        let last_send_instant = Utc::now();

        let sem_clone = semaphore.clone();

        let q = q.clone();

        client.clone().update_callback(Arc::downgrade(&request), Box::new(move |reply| {

            //Release another request for this client
            sem_clone.release();

            let latency = Utc::now()
                .signed_duration_since(last_send_instant)
                .num_nanoseconds()
                .unwrap_or(i64::MAX);

            let time_ms = Utc::now().timestamp_millis();

            /*let _ = q.enqueue(
                format!(
                    "{}\t{}\t{}\n",
                    id,
                    time_ms,
                    latency,
                ),
            );*/

            if MicrobenchmarkData::VERBOSE {
                if req % 1000 == 0 {
                    println!("{} // {} operations sent!", id, req);
                }

                println!(" sent!");
            }
        }));

        if MicrobenchmarkData::REQUEST_SLEEP_MILLIS != Duration::ZERO {
            std::thread::sleep(MicrobenchmarkData::REQUEST_SLEEP_MILLIS);
        } else if ramp_up > 0 {
            std::thread::sleep(Duration::from_millis(ramp_up as u64));

            ramp_up -= 100;
        }

    }

    println!("Executing experiment for {} ops", MicrobenchmarkData::OPS_NUMBER / 2);

    let iterator = 0..(MicrobenchmarkData::OPS_NUMBER / 2 / concurrent_rqs);

    //let mut st = BenchmarkHelper::new(client.id(), MicrobenchmarkData::OPS_NUMBER / 2);

    for req in iterator {
        semaphore.acquire();

        if MicrobenchmarkData::VERBOSE {
            print!("Sending req {}...", req);
        }

        let q = q.clone();

        let last_send_instant = Utc::now();

        let sem_clone = semaphore.clone();

        client.clone().update_callback(Arc::downgrade(&request),
                               Box::new(move |reply| {

            //Release another request for this client
            sem_clone.release();

            /* let latency = Utc::now()
                .signed_duration_since(last_send_instant)
                .num_nanoseconds()
                .unwrap_or(i64::MAX);

            let time_ms = Utc::now().timestamp_millis();*/

            /*let _ = q.enqueue(
                format!(
                    "{}\t{}\t{}\n",
                    id,
                    time_ms,
                    latency,
                ),
            );*/

            //(exec_time, last_send_instant).store(st);

            if MicrobenchmarkData::VERBOSE {
                if req % 1000 == 0 {
                    println!("{} // {} operations sent!", id, req);
                }

                println!(" sent!");
            }
        }));

        if MicrobenchmarkData::REQUEST_SLEEP_MILLIS != Duration::ZERO {
            std::thread::sleep(MicrobenchmarkData::REQUEST_SLEEP_MILLIS);
        }
    }

    //Wait for all requests to finish
    for _ in 0..concurrent_rqs {
        semaphore.acquire();
    }

    println!("{:?} // Done.", client.id());

    /*
    if id == 1000 {
        println!("{} // Average time for {} executions (-10%) = {} us",
                 id,
                 MicrobenchmarkData::OPS_NUMBER / 2,
                 st.average(true, false) / 1000.0);

        println!("{} // Standard deviation for {} executions (-10%) = {} us",
                 id,
                 MicrobenchmarkData::OPS_NUMBER / 2,
                 st.standard_deviation(true, true) / 1000.0);

        println!("{} // Average time for {} executions (all samples) = {} us",
                 id,
                 MicrobenchmarkData::OPS_NUMBER / 2,
                 st.average(false, true) / 1000.0);

        println!("{} // Standard deviation for {} executions (all samples) = {} us",
                 id,
                 MicrobenchmarkData::OPS_NUMBER / 2,
                 st.standard_deviation(false, true) / 1000.0);

        println!("{} // Maximum time for {} executions (all samples) = {} us",
                 id,
                 MicrobenchmarkData::OPS_NUMBER / 2,
                 st.max(false) / 1000);
    }
    */
}
