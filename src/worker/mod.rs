mod context;
mod processor;

use std::time::Duration;

pub use processor::Processor;

pub async fn run(processor: Processor, mut shutdown: tokio::sync::watch::Receiver<bool>) {
    let poll_interval = processor.poll_interval();
    let mut cleanup_interval = tokio::time::interval(Duration::from_secs(60 * 60));
    cleanup_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
            _ = cleanup_interval.tick() => {
                if let Err(error) = processor.cleanup().await {
                    tracing::warn!(%error, "persistence cleanup failed");
                }
            }
            result = processor.process_next() => {
                match result {
                    Ok(true) => {}
                    Ok(false) => tokio::time::sleep(poll_interval).await,
                    Err(error) => {
                        tracing::error!(%error, "job worker iteration failed");
                        tokio::time::sleep(poll_interval).await;
                    }
                }
            }
        }
    }
    tracing::info!("job worker stopped");
}
