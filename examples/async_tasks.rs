/// Example: Concurrent async tasks with tokio
///
/// Run with:
///   cargo run --example async_tasks
use std::time::Duration;
use tokio::time::sleep;

#[derive(Debug)]
struct TaskResult {
    duration_ms: u64,
    output: String,
}

async fn simulate_task(name: &str, duration_ms: u64) -> TaskResult {
    sleep(Duration::from_millis(duration_ms)).await;
    TaskResult {
        duration_ms,
        output: format!("Task '{}' completed in {}ms", name, duration_ms),
    }
}

#[tokio::main]
async fn main() {
    println!("Launching tasks concurrently...\n");

    let start = std::time::Instant::now();

    // Run all tasks concurrently instead of sequentially
    let (r1, r2, r3) = tokio::join!(
        simulate_task("fetch-config", 300),
        simulate_task("load-cache", 150),
        simulate_task("ping-service", 200),
    );

    let elapsed = start.elapsed().as_millis();

    for result in [&r1, &r2, &r3] {
        println!("✓ {}", result.output);
    }

    println!("\nAll tasks finished in {}ms total", elapsed);
    println!(
        "(Sequential would have taken {}ms)",
        r1.duration_ms + r2.duration_ms + r3.duration_ms
    );
}
