use std::default::Default;
use std::sync::{Arc, Weak};

use chrono::DateTime;
use chrono::offset::Utc;

use febft::bft::benchmarks::{
    BatchMeta,
    BenchmarkHelperStore,
    Measurements,
};
use febft::bft::communication::NodeId;
use febft::bft::error::*;
use febft::bft::executable::{
    Service,
    UpdateBatch,
    UpdateBatchReplies,
};

use crate::serialize::MicrobenchmarkData;

pub struct Microbenchmark {
    id: NodeId,
    max_tp: f32,
    max_tp_time: DateTime<Utc>,
    iterations: usize,
    reply: Arc<Vec<u8>>,
    measurements: Measurements,
}

impl Microbenchmark {
    pub fn new(id: NodeId) -> Self {
        let reply = Arc::new((0..)
            .into_iter()
            .take(MicrobenchmarkData::REPLY_SIZE)
            .map(|x| (x & 0xff) as u8)
            .collect());

        Self {
            id: id.clone(),
            reply,
            max_tp: -1.0,
            max_tp_time: Utc::now(),
            iterations: 0,
            measurements: Measurements::new(id),
        }
    }
}

impl Service for Microbenchmark {
    type Data = MicrobenchmarkData;

    fn initial_state(&mut self) -> Result<Vec<u8>> {
        // TODO: remove this
        drop(MicrobenchmarkData::REQUEST_SIZE);

        Ok((0..)
            .into_iter()
            .take(MicrobenchmarkData::STATE_SIZE)
            .map(|x| (x & 0xff) as u8)
            .collect())
    }

    fn update(&mut self, _s: &mut Vec<u8>, _r: Weak<Vec<u8>>) -> Weak<Vec<u8>> {
        unimplemented!()
    }

    fn update_batch(
        &mut self,
        _state: &mut Vec<u8>,
        batch: UpdateBatch<Weak<Vec<u8>>>,
        mut meta: BatchMeta,
    ) -> UpdateBatchReplies<Weak<Vec<u8>>> {
        let batch_len = batch.len();
        
        let mut reply_batch = UpdateBatchReplies::with_capacity(batch.len());

        for update in batch.into_inner() {
            let (peer_id, sess, opid, _req) = update.into_inner();
            let reply = Arc::downgrade(&self.reply);
            reply_batch.add(peer_id, sess, opid, reply);
        }

        meta.execution_time = Utc::now();

        // take measurements
        meta.batch_size.store(&mut self.measurements.batch_size);
        (meta.consensus_decision_time, meta.consensus_start_time).store(&mut self.measurements.consensus_latency);
        (meta.reception_time, meta.consensus_start_time).store(&mut self.measurements.pre_cons_latency);
        (meta.execution_time, meta.consensus_decision_time).store(&mut self.measurements.pos_cons_latency);
        (meta.prepare_sent_time, meta.consensus_start_time).store(&mut self.measurements.pre_prepare_latency);
        (meta.commit_sent_time, meta.prepare_sent_time).store(&mut self.measurements.prepare_latency);
        (meta.consensus_decision_time, meta.commit_sent_time).store(&mut self.measurements.commit_latency);
        (meta.prepare_sent_time, meta.pre_prepare_received_time).store(&mut self.measurements.prepare_msg_latency);
        (meta.done_propose, meta.started_propose).store(&mut self.measurements.propose_time_latency);
        (meta.commit_sent_time, meta.first_prepare_received).store(&mut self.measurements.prepare_time_taken);
        (meta.consensus_decision_time, meta.first_commit_received).store(&mut self.measurements.commit_time_taken);

        meta.message_passing_latencies.store(&mut self.measurements.message_sending_time_taken);
        meta.message_signing_latencies.store(&mut self.measurements.message_signing_time_taken);
        meta.message_send_to_create.store(&mut self.measurements.message_send_to_create);
        meta.message_passing_latencies_own.store(&mut self.measurements.message_sending_time_taken_own);

        for _ in 0..batch_len {
            // increase iter count
            self.iterations += 1;

            if self.iterations % MicrobenchmarkData::MEASUREMENT_INTERVAL == 0 {
                println!("{:?} // --- Measurements after {} ops ({} samples) ---",
                         self.id, self.iterations, MicrobenchmarkData::MEASUREMENT_INTERVAL);

                let diff = Utc::now()
                    .signed_duration_since(self.max_tp_time)
                    .num_microseconds().expect("Need micro seconds");

                let tp = (MicrobenchmarkData::MEASUREMENT_INTERVAL as f32 * 1000.0 * 1000.0) / (diff as f32);

                if tp > self.max_tp {
                    self.max_tp = tp;
                }

                self.max_tp_time = Utc::now();

                println!("{:?} // Throughput = {} operations/sec (Maximum observed: {} ops/sec)",
                         self.id, tp, self.max_tp);

                self.measurements.total_latency.log_latency("Total");
                self.measurements.consensus_latency.log_latency("Consensus");
                self.measurements.pre_cons_latency.log_latency("Pre-consensus");
                self.measurements.pos_cons_latency.log_latency("Pos-consensus");
                self.measurements.pre_prepare_latency.log_latency("Propose / PrePrepare");
                self.measurements.prepare_latency.log_latency("Write / Prepare");
                self.measurements.commit_latency.log_latency("Accept / Commit");
                self.measurements.prepare_msg_latency.log_latency("Prepare msg");
                self.measurements.propose_time_latency.log_latency("Propose time");
                self.measurements.prepare_time_taken.log_latency("Prepare time taken");
                self.measurements.commit_time_taken.log_latency("Commit time taken");
                self.measurements.message_sending_time_taken_own.log_latency("Message sending (Own)");
                self.measurements.message_sending_time_taken.log_latency("Message sending");
                self.measurements.message_signing_time_taken.log_latency("Message signing");
                self.measurements.message_send_to_create.log_latency("Create send to objects");
                self.measurements.batch_size.log_batch();

            }
        }

        reply_batch
    }
}
