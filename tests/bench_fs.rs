use std::time::Instant;
use tokio::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bench_fs_starvation() {
    let content = "a".repeat(1024 * 1024 * 10); // 10MB file
    std::fs::write("test_large_file.txt", &content).unwrap();

    // 1. Benchmark blocking std::fs::read_to_string
    let start = Instant::now();
    let mut tasks = vec![];
    for _ in 0..10 {
        tasks.push(tokio::spawn(async {
            // Blocking I/O inside async block
            let _ = std::fs::read_to_string("test_large_file.txt").unwrap();
        }));
    }

    // Concurrently try to do some other async work
    let async_work = tokio::spawn(async {
        let start = Instant::now();
        tokio::time::sleep(Duration::from_millis(10)).await;
        println!("Async sleep took (std::fs): {:?}", start.elapsed());
    });

    for task in tasks {
        let _ = task.await;
    }
    let _ = async_work.await;
    let elapsed = start.elapsed();
    println!("Total elapsed with std::fs (blocking): {:?}", elapsed);

    // 2. Benchmark async tokio::fs::read_to_string
    let start2 = Instant::now();
    let mut tasks2 = vec![];
    for _ in 0..10 {
        tasks2.push(tokio::spawn(async {
            // Async I/O inside async block
            let _ = tokio::fs::read_to_string("test_large_file.txt")
                .await
                .unwrap();
        }));
    }

    // Concurrently try to do some other async work
    let async_work2 = tokio::spawn(async {
        let start = Instant::now();
        tokio::time::sleep(Duration::from_millis(10)).await;
        println!("Async sleep took (tokio::fs): {:?}", start.elapsed());
    });

    for task in tasks2 {
        let _ = task.await;
    }
    let _ = async_work2.await;
    let elapsed2 = start2.elapsed();
    println!("Total elapsed with tokio::fs (async): {:?}", elapsed2);

    std::fs::remove_file("test_large_file.txt").unwrap();
}
