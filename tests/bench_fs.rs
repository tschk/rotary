use std::time::Instant;
use tokio::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bench_fs_starvation() {
    let content = "a".repeat(1024 * 1024 * 10); // 10MB file
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    std::fs::write(&path, &content).unwrap();

    // 1. Benchmark blocking std::fs::read_to_string
    let start = Instant::now();
    let mut tasks = vec![];
    for _ in 0..10 {
        let path = path.clone();
        tasks.push(tokio::spawn(async move {
            // Blocking I/O inside async block
            let _ = std::fs::read_to_string(&path).unwrap();
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
        let path = path.clone();
        tasks2.push(tokio::spawn(async move {
            // Async I/O inside async block
            let _ = tokio::fs::read_to_string(&path).await.unwrap();
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
}
